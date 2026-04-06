use super::*;
use modkit_macros::domain_model;
use serde_json::json;
use std::sync::atomic::{AtomicBool, Ordering};
use uuid::Uuid;

#[domain_model]
struct MockRepo {
    is_ready: AtomicBool,
    fail_switch: bool,
}

impl MockRepo {
    fn new() -> Self {
        Self {
            is_ready: AtomicBool::new(false),
            fail_switch: false,
        }
    }

    fn with_fail_switch() -> Self {
        Self {
            is_ready: AtomicBool::new(false),
            fail_switch: true,
        }
    }
}

impl GtsRepository for MockRepo {
    fn register(
        &self,
        entity: &serde_json::Value,
        _validate: bool,
    ) -> Result<GtsEntity, DomainError> {
        let gts_id = entity
            .get("$id")
            .and_then(|v| v.as_str())
            .ok_or_else(|| DomainError::invalid_gts_id("No $id field"))?;

        if gts_id.contains("fail") {
            return Err(DomainError::validation_failed("Test failure"));
        }

        Ok(GtsEntity::new(
            Uuid::nil(),
            gts_id.to_owned(),
            vec![],
            true, // is_schema
            entity.clone(),
            None,
        ))
    }

    fn get(&self, gts_id: &str) -> Result<GtsEntity, DomainError> {
        if gts_id.contains("notfound") {
            return Err(DomainError::not_found(gts_id));
        }
        Ok(GtsEntity::new(
            Uuid::nil(),
            gts_id.to_owned(),
            vec![],
            true, // is_schema
            json!({}),
            None,
        ))
    }

    fn list(&self, _query: &ListQuery) -> Result<Vec<GtsEntity>, DomainError> {
        Ok(vec![GtsEntity::new(
            Uuid::nil(),
            "gts.test.pkg.ns.type.v1~".to_owned(),
            vec![],
            true, // is_schema
            json!({}),
            None,
        )])
    }

    fn exists(&self, _gts_id: &str) -> bool {
        true
    }

    fn is_ready(&self) -> bool {
        self.is_ready.load(Ordering::SeqCst)
    }

    fn switch_to_ready(&self) -> Result<(), Vec<String>> {
        if self.fail_switch {
            // Return errors in "gts_id: message" format for ValidationError::from_string
            return Err(vec![
                "gts.test1~: error1".to_owned(),
                "gts.test2~: error2".to_owned(),
            ]);
        }
        self.is_ready.store(true, Ordering::SeqCst);
        Ok(())
    }
}

#[test]
fn test_extract_gts_id() {
    let service = TypesRegistryService::new(
        Arc::new(MockRepo::new()),
        crate::config::TypesRegistryConfig::default(),
    );

    let entity = json!({"$id": "gts://gts.acme.core.events.test.v1~"});
    assert_eq!(
        service.extract_gts_id(&entity),
        Some("gts.acme.core.events.test.v1~".to_owned())
    );

    let entity = json!({"gtsId": "gts.acme.core.events.test.v1~"});
    assert_eq!(
        service.extract_gts_id(&entity),
        Some("gts.acme.core.events.test.v1~".to_owned())
    );

    let entity = json!({"id": "gts.acme.core.events.test.v1~"});
    assert_eq!(
        service.extract_gts_id(&entity),
        Some("gts.acme.core.events.test.v1~".to_owned())
    );

    let entity = json!({"other": "value"});
    assert_eq!(service.extract_gts_id(&entity), None);

    let entity = json!("not an object");
    assert_eq!(service.extract_gts_id(&entity), None);
}

#[test]
fn test_register_success() {
    let service = TypesRegistryService::new(
        Arc::new(MockRepo::new()),
        crate::config::TypesRegistryConfig::default(),
    );

    let entities = vec![
        json!({"$id": "gts://gts.acme.core.events.test.v1~"}),
        json!({"$id": "gts://gts.acme.core.events.test2.v1~"}),
    ];

    let results = service.register(entities);
    assert_eq!(results.len(), 2);
    assert!(results[0].is_ok());
    assert!(results[1].is_ok());
}

#[test]
fn test_register_with_failures() {
    let service = TypesRegistryService::new(
        Arc::new(MockRepo::new()),
        crate::config::TypesRegistryConfig::default(),
    );

    let entities = vec![
        json!({"$id": "gts://gts.acme.core.events.test.v1~"}),
        json!({"$id": "gts://gts.acme.core.events.fail.v1~"}),
        json!({"other": "no id"}),
    ];

    let results = service.register(entities);
    assert_eq!(results.len(), 3);
    assert!(results[0].is_ok());
    assert!(results[1].is_err());
    assert!(results[2].is_err());
}

#[test]
fn test_get_success() {
    let service = TypesRegistryService::new(
        Arc::new(MockRepo::new()),
        crate::config::TypesRegistryConfig::default(),
    );
    let result = service.get("gts.acme.core.events.test.v1~");
    assert!(result.is_ok());
}

#[test]
fn test_get_not_found() {
    let service = TypesRegistryService::new(
        Arc::new(MockRepo::new()),
        crate::config::TypesRegistryConfig::default(),
    );
    let result = service.get("gts.notfound.pkg.ns.type.v1~");
    assert!(result.is_err());
}

#[test]
fn test_list() {
    let service = TypesRegistryService::new(
        Arc::new(MockRepo::new()),
        crate::config::TypesRegistryConfig::default(),
    );
    let result = service.list(&ListQuery::default());
    assert!(result.is_ok());
    assert_eq!(result.unwrap().len(), 1);
}

#[test]
fn test_switch_to_ready_success() {
    let service = TypesRegistryService::new(
        Arc::new(MockRepo::new()),
        crate::config::TypesRegistryConfig::default(),
    );
    assert!(!service.is_ready());

    let result = service.switch_to_ready();
    assert!(result.is_ok());
    assert!(service.is_ready());
}

#[test]
fn test_switch_to_ready_failure() {
    let service = TypesRegistryService::new(
        Arc::new(MockRepo::with_fail_switch()),
        crate::config::TypesRegistryConfig::default(),
    );
    let result = service.switch_to_ready();
    assert!(result.is_err());
    match result.unwrap_err() {
        DomainError::ReadyCommitFailed(errors) => {
            assert_eq!(errors.len(), 2);
            assert_eq!(errors[0].gts_id, "gts.test1~");
            assert_eq!(errors[0].message, "error1");
            assert_eq!(errors[1].gts_id, "gts.test2~");
            assert_eq!(errors[1].message, "error2");
        }
        _ => panic!("Expected ReadyCommitFailed"),
    }
}

#[test]
fn test_is_ready() {
    let service = TypesRegistryService::new(
        Arc::new(MockRepo::new()),
        crate::config::TypesRegistryConfig::default(),
    );
    assert!(!service.is_ready());
}
