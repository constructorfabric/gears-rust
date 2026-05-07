//! Unit-style tests for the test-only microchat module's validators
//! and quota check. Tests #1–12 are pure (no DB); tests #13–16 own a
//! `:memory:` SQLite with the `chat_attachments` migration applied.

use microchat_test::{
    AttachmentStatus, MicrochatError, MicrochatLimits, MicrochatRepo, MIME_ALLOWLIST,
    Migrator as MicrochatMigrator, validate_filename, validate_mime,
};
use modkit_db::secure::Db;
use modkit_db::{ConnectOpts, connect_db};
use modkit_db::migration_runner::run_migrations_for_testing;
use sea_orm_migration::MigratorTrait;
use time::OffsetDateTime;
use uuid::Uuid;

fn limits() -> MicrochatLimits {
    MicrochatLimits::default()
}

// ── Tests #1–12 — pure validators ──────────────────────────────────────────

#[test]
fn mime_allowlist_accepts_each_known_mime() {
    let lim = limits();
    for &mime in MIME_ALLOWLIST {
        validate_mime(mime, &lim).unwrap_or_else(|e| panic!("expected {mime} to pass: {e}"));
    }
}

#[test]
fn mime_rejects_unknown_application_octet_stream() {
    let lim = limits();
    let err = validate_mime("application/octet-stream", &lim).unwrap_err();
    assert!(matches!(err, MicrochatError::MimeNotAllowed(_)), "got {err:?}");
}

#[test]
fn mime_rejects_html() {
    let lim = limits();
    let err = validate_mime("text/html", &lim).unwrap_err();
    assert!(matches!(err, MicrochatError::MimeNotAllowed(_)), "got {err:?}");
}

#[test]
fn mime_strips_params_before_compare() {
    let lim = limits();
    validate_mime("text/plain; charset=utf-8", &lim).expect("plain+param accepted");
    // Case-insensitive on the bare type.
    validate_mime("Text/Plain;CHARSET=UTF-8", &lim).expect("case-insensitive accepted");
}

#[test]
fn filename_rejects_empty() {
    let lim = limits();
    let err = validate_filename("", &lim).unwrap_err();
    assert!(matches!(err, MicrochatError::InvalidFilename(_)), "got {err:?}");
}

#[test]
fn filename_rejects_too_long() {
    let lim = limits();
    let too_long: String = std::iter::repeat('x').take(lim.max_filename_len + 1).collect();
    let err = validate_filename(&too_long, &lim).unwrap_err();
    assert!(matches!(err, MicrochatError::InvalidFilename(_)), "got {err:?}");
    let exact: String = std::iter::repeat('x').take(lim.max_filename_len).collect();
    validate_filename(&exact, &lim).expect("exact-length filename accepted");
}

#[test]
fn filename_rejects_path_traversal() {
    let lim = limits();
    let err = validate_filename("../etc/passwd", &lim).unwrap_err();
    assert!(matches!(err, MicrochatError::InvalidFilename(_)), "got {err:?}");
    // even without the slash, a literal `..` in the name is rejected
    let err = validate_filename("foo..bar.pdf", &lim).unwrap_err();
    assert!(matches!(err, MicrochatError::InvalidFilename(_)), "got {err:?}");
}

#[test]
fn filename_rejects_forward_slash() {
    let lim = limits();
    let err = validate_filename("a/b.pdf", &lim).unwrap_err();
    assert!(matches!(err, MicrochatError::InvalidFilename(_)), "got {err:?}");
}

#[test]
fn filename_rejects_backslash() {
    let lim = limits();
    let err = validate_filename("a\\b.pdf", &lim).unwrap_err();
    assert!(matches!(err, MicrochatError::InvalidFilename(_)), "got {err:?}");
}

#[test]
fn filename_rejects_control_char() {
    let lim = limits();
    let err = validate_filename("foo\x07.pdf", &lim).unwrap_err();
    assert!(matches!(err, MicrochatError::InvalidFilename(_)), "got {err:?}");
}

#[test]
fn filename_rejects_leading_whitespace() {
    let lim = limits();
    let err = validate_filename(" doc.pdf", &lim).unwrap_err();
    assert!(matches!(err, MicrochatError::InvalidFilename(_)), "got {err:?}");
    let err = validate_filename("doc.pdf ", &lim).unwrap_err();
    assert!(matches!(err, MicrochatError::InvalidFilename(_)), "got {err:?}");
}

#[test]
fn filename_accepts_unicode_letters() {
    let lim = limits();
    validate_filename("отчёт.pdf", &lim).expect("unicode letters accepted");
}

// ── Tests #13–16 — quota (DB-backed) ───────────────────────────────────────

async fn fresh_db() -> Db {
    let db = connect_db("sqlite::memory:", ConnectOpts::default())
        .await
        .expect("connect sqlite::memory:");
    run_migrations_for_testing(&db, MicrochatMigrator::migrations())
        .await
        .expect("run microchat migrations");
    db
}

async fn seed_row(repo: &MicrochatRepo, db: &Db, owner_id: Uuid, status: AttachmentStatus) {
    let conn = db.conn().expect("conn");
    let file_id = Uuid::new_v4();
    repo.insert_pending(
        &conn,
        file_id,
        Uuid::new_v4(),
        owner_id,
        "doc.pdf",
        "application/pdf",
        OffsetDateTime::now_utc(),
    )
    .await
    .expect("insert_pending");
    match status {
        AttachmentStatus::Pending => {}
        AttachmentStatus::Active => repo
            .mark_active(&conn, file_id, &"etag".to_string(), 0)
            .await
            .expect("mark_active"),
        AttachmentStatus::Deleted => repo
            .mark_deleted(&conn, file_id)
            .await
            .expect("mark_deleted"),
    }
}

#[tokio::test(flavor = "current_thread")]
async fn quota_under_limit_passes() {
    let db = fresh_db().await;
    let repo = MicrochatRepo::new();
    let owner = Uuid::new_v4();
    let lim = limits();

    for _ in 0..(lim.max_files_per_user - 1) {
        seed_row(&repo, &db, owner, AttachmentStatus::Active).await;
    }
    let conn = db.conn().expect("conn");
    repo.enforce_quota(&conn, owner, lim.max_files_per_user)
        .await
        .expect("under limit accepted");
}

#[tokio::test(flavor = "current_thread")]
async fn quota_at_limit_rejects() {
    let db = fresh_db().await;
    let repo = MicrochatRepo::new();
    let owner = Uuid::new_v4();
    let lim = limits();

    for _ in 0..lim.max_files_per_user {
        seed_row(&repo, &db, owner, AttachmentStatus::Active).await;
    }
    let conn = db.conn().expect("conn");
    let err = repo
        .enforce_quota(&conn, owner, lim.max_files_per_user)
        .await
        .unwrap_err();
    assert!(
        matches!(err, MicrochatError::QuotaExceeded { max } if max == lim.max_files_per_user),
        "got {err:?}"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn quota_counts_pending_against_limit() {
    let db = fresh_db().await;
    let repo = MicrochatRepo::new();
    let owner = Uuid::new_v4();
    let lim = limits();

    for _ in 0..lim.max_files_per_user {
        seed_row(&repo, &db, owner, AttachmentStatus::Pending).await;
    }
    let conn = db.conn().expect("conn");
    let err = repo
        .enforce_quota(&conn, owner, lim.max_files_per_user)
        .await
        .unwrap_err();
    assert!(
        matches!(err, MicrochatError::QuotaExceeded { max } if max == lim.max_files_per_user),
        "got {err:?}"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn quota_does_not_count_deleted() {
    let db = fresh_db().await;
    let repo = MicrochatRepo::new();
    let owner = Uuid::new_v4();
    let lim = limits();

    // 5 deleted + 1 active = 1 chargeable row.
    for _ in 0..lim.max_files_per_user {
        seed_row(&repo, &db, owner, AttachmentStatus::Deleted).await;
    }
    seed_row(&repo, &db, owner, AttachmentStatus::Active).await;

    let conn = db.conn().expect("conn");
    assert_eq!(
        repo.count_active_for_owner(&conn, owner)
            .await
            .expect("count"),
        1,
        "deleted rows must not be counted"
    );
    repo.enforce_quota(&conn, owner, lim.max_files_per_user)
        .await
        .expect("only deleted+1 active → still under limit");
}
