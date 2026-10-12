//! Decimal regressions for the pure payment family; no database or orchestration substitutes.
use bss_ledger_sdk::{AccountClass, CurrencySpec, MoneyError, PostedMoney, Side};
use rust_decimal::Decimal;
use uuid::Uuid;

use super::{SettlementInput, build_settlement_entry};
use crate::domain::error::DomainError;
use crate::domain::exact_money::{ExactAmount, subtract_posted};
use crate::domain::payment::{allocation, credit, precedence, settlement_return};

fn money(value: &str, code: &str, scale: u8) -> PostedMoney {
    PostedMoney::try_new(
        value.parse::<Decimal>().unwrap(),
        CurrencySpec::try_new(code.to_owned(), scale).unwrap(),
    )
    .unwrap()
}
fn eur(value: &str) -> PostedMoney {
    money(value, "EUR", 2)
}
fn candidate(id: &str, open: PostedMoney) -> precedence::Candidate {
    precedence::Candidate {
        invoice_id: id.to_owned(),
        open,
        original_posted_at: None,
    }
}
fn share(id: &str, amount: PostedMoney) -> precedence::Allocated {
    precedence::Allocated {
        invoice_id: id.to_owned(),
        amount,
    }
}
fn subgrain(id: &str, available: PostedMoney) -> credit::CreditSubgrain {
    credit::CreditSubgrain {
        credit_grant_event_type: id.to_owned(),
        available,
    }
}
fn settle(gross: PostedMoney, fee: PostedMoney) -> SettlementInput {
    SettlementInput {
        tenant_id: Uuid::now_v7(),
        payer_tenant_id: Uuid::now_v7(),
        payment_id: "P".to_owned(),
        gross,
        fee,
        effective_at: None,
    }
}
fn allocation_input(splits: Vec<precedence::Allocated>) -> allocation::AllocationInput {
    allocation::AllocationInput {
        tenant_id: Uuid::now_v7(),
        payer_tenant_id: Uuid::now_v7(),
        payment_id: "P".to_owned(),
        allocation_id: Uuid::now_v7(),
        currency: eur("0").currency().clone(),
        splits,
        effective_at: None,
    }
}
fn return_input(amount: PostedMoney) -> settlement_return::SettlementReturnInput {
    settlement_return::SettlementReturnInput {
        tenant_id: Uuid::now_v7(),
        payer_tenant_id: Uuid::now_v7(),
        payment_id: "P".to_owned(),
        psp_return_id: "R".to_owned(),
        amount,
        effective_at: None,
    }
}
fn apply_input(
    debits: Vec<credit::CreditDebit>,
    targets: Vec<precedence::Allocated>,
) -> credit::ApplyInput {
    credit::ApplyInput {
        tenant_id: Uuid::now_v7(),
        payer_tenant_id: Uuid::now_v7(),
        credit_application_id: "C".to_owned(),
        currency: eur("0").currency().clone(),
        debits,
        targets,
        effective_at: None,
    }
}
fn balanced(entry: &bss_ledger_sdk::PostEntry) {
    let total = |side| {
        entry.lines.iter().filter(|l| l.side == side).fold(
            ExactAmount::from_decimal(Decimal::ZERO),
            |sum, line| {
                sum.checked_add(&ExactAmount::from_decimal(line.money.amount()))
                    .unwrap()
            },
        )
    };
    assert_eq!(total(Side::Debit), total(Side::Credit));
}

#[test]
fn fractional_settle_allocate_and_supplied_fee_return_balance() {
    let input = settle(eur("12.34"), eur("0.34"));
    let entry = build_settlement_entry(&input).unwrap();
    assert_eq!(entry.lines[0].money, eur("12"));
    assert_eq!(entry.lines[1].money, eur("0.34"));
    assert_eq!(entry.lines[2].money, eur("12.34"));
    assert_eq!(entry.lines[0].account_class, AccountClass::CashClearing);
    balanced(&entry);
    let candidates = [candidate("I", eur("12.34"))];
    let shares = precedence::oldest_first(&candidates, &eur("10"), None).unwrap();
    let allocated = allocation::build_allocation_entry(&allocation_input(shares)).unwrap();
    assert_eq!(allocated.lines[0].money, eur("10"));
    assert_eq!(
        subtract_posted(&input.gross, &allocated.lines[0].money).unwrap(),
        eur("2.34")
    );
    balanced(&allocated);
    // Pure return uses the owner's supplied share, never recomputes a ratio.
    let returned =
        settlement_return::build_settlement_return_entry(&return_input(eur("2.34")), &eur("0.06"))
            .unwrap();
    assert_eq!(returned.lines[1].money, eur("2.28"));
    assert_eq!(returned.lines[2].money, eur("0.06"));
    balanced(&returned);
    for line in entry
        .lines
        .iter()
        .chain(&allocated.lines)
        .chain(&returned.lines)
    {
        assert_eq!(line.money.currency(), eur("0").currency());
        assert_eq!(line.functional_money, None);
    }
    assert!(matches!(
        PostedMoney::try_new("0.047".parse().unwrap(), eur("0").currency().clone()),
        Err(MoneyError::InvalidPostingIncrement)
    ));
}

#[test]
fn settlement_and_return_validate_zero_fee_metadata() {
    for bad in [money("0", "USD", 2), money("0", "EUR", 3)] {
        let err = build_settlement_entry(&settle(eur("1"), bad.clone())).unwrap_err();
        let ret_err =
            settlement_return::build_settlement_return_entry(&return_input(eur("1")), &bad)
                .unwrap_err();
        if bad.currency().code() == "USD" {
            assert!(matches!(err, DomainError::CurrencyMismatch(_)));
            assert!(matches!(ret_err, DomainError::CurrencyMismatch(_)));
        } else {
            assert!(matches!(err, DomainError::InconsistentScale(_)));
            assert!(matches!(ret_err, DomainError::InconsistentScale(_)));
        }
    }
}

#[test]
fn precedence_validates_skipped_and_unreached_candidates_and_zero_lump() {
    for value in ["0", "-1", "1"] {
        for bad in [money(value, "USD", 2), money(value, "EUR", 3)] {
            let candidates = [candidate("A", eur("1")), candidate("Z", bad.clone())];
            for lump in [eur("1"), eur("0"), eur("-1")] {
                for strategy in [
                    precedence::PrecedenceStrategy::OldestFirst,
                    precedence::PrecedenceStrategy::HighestAmountFirst,
                ] {
                    let err = precedence::select_split(&candidates, &lump, Some("A"), strategy)
                        .unwrap_err();
                    if bad.currency().code() == "USD" {
                        assert!(matches!(err, DomainError::CurrencyMismatch(_)));
                    } else {
                        assert!(matches!(err, DomainError::InconsistentScale(_)));
                    }
                }
            }
        }
    }
}

#[test]
fn wallet_validates_skipped_and_after_exhaustion_metadata() {
    for value in ["0", "-1", "1"] {
        for bad in [money(value, "USD", 2), money(value, "EUR", 3)] {
            let grains = [subgrain("A", eur("1")), subgrain("Z", bad.clone())];
            for amount in [eur("1"), eur("0")] {
                let err = credit::plan_wallet_debit(&grains, &amount).unwrap_err();
                if bad.currency().code() == "USD" {
                    assert!(matches!(err, DomainError::CurrencyMismatch(_)));
                } else {
                    assert!(matches!(err, DomainError::InconsistentScale(_)));
                }
            }
        }
    }
}

#[test]
fn caller_and_credit_targets_validate_all_candidates_even_empty_shares() {
    let candidates = [
        candidate("A", eur("1")),
        candidate("Z", money("0", "EUR", 3)),
    ];
    for targets in [vec![], vec![share("A", eur("1"))]] {
        assert!(matches!(
            allocation::validate_caller_split(&candidates, &targets, &eur("1")),
            Err(DomainError::InconsistentScale(_))
        ));
        assert!(matches!(
            credit::validate_credit_targets(&candidates, &targets),
            Err(DomainError::InconsistentScale(_))
        ));
    }
    assert!(
        credit::validate_credit_targets(&[], &[])
            .unwrap()
            .is_empty()
    );
    assert!(
        precedence::oldest_first(&[], &money("1", "JPY", 0), None)
            .unwrap()
            .is_empty()
    );
}

#[test]
fn builder_metadata_precedes_zero_share_and_zero_debit_policy() {
    assert!(matches!(
        allocation::build_allocation_entry(&allocation_input(vec![
            share("A", eur("0")),
            share("Z", money("0", "USD", 2))
        ])),
        Err(DomainError::CurrencyMismatch(_))
    ));
    let debits = vec![credit::CreditDebit {
        credit_grant_event_type: "G".to_owned(),
        amount: eur("0"),
    }];
    assert!(matches!(
        credit::build_apply_entry(&apply_input(debits, vec![share("Z", money("0", "EUR", 3))])),
        Err(DomainError::InconsistentScale(_))
    ));
}

#[test]
fn wide_remaining_intermediates_allow_final_shares_that_fit() {
    let huge = eur("1000000000000000000000000000");
    let large_final = eur("999999999999999999999999999");
    // huge - .01 has 30 normalized coefficient digits, but every final share fits.
    assert!(subtract_posted(&huge, &eur("0.01")).is_err());
    let candidates = [
        candidate("A", eur("0.01")),
        candidate("B", eur("0.99")),
        candidate("C", large_final.clone()),
    ];
    let shares = precedence::oldest_first(&candidates, &huge, None).unwrap();
    assert_eq!(
        shares,
        vec![
            share("A", eur("0.01")),
            share("B", eur("0.99")),
            share("C", large_final.clone())
        ]
    );
    let grains = [
        subgrain("A", eur("0.01")),
        subgrain("B", eur("0.99")),
        subgrain("C", large_final),
    ];
    let debits = credit::plan_wallet_debit(&grains, &huge).unwrap();
    assert_eq!(
        debits.iter().map(|d| d.amount.clone()).collect::<Vec<_>>(),
        shares.iter().map(|s| s.amount.clone()).collect::<Vec<_>>()
    );
    assert!(allocation::validate_caller_split(&candidates, &shares, &huge).is_ok());
    let entry = allocation::build_allocation_entry(&allocation_input(shares.clone())).unwrap();
    assert_eq!(entry.lines[0].money, huge);
    balanced(&entry);
    balanced(&credit::build_apply_entry(&apply_input(debits, shares)).unwrap());
}

#[test]
fn wallet_capacity_and_caller_cap_remain_exact_above_posted_bound() {
    let max = eur("9999999999999999999999999999");
    let grains = [
        subgrain("A", max.clone()),
        subgrain("B", max.clone()),
        subgrain("negative", eur("-1")),
    ];
    let debits = credit::plan_wallet_debit(&grains, &eur("1")).unwrap();
    assert_eq!(debits.len(), 1);
    assert_eq!(debits[0].amount, eur("1"));
    let candidates = [candidate("A", max.clone()), candidate("B", max.clone())];
    let shares = [share("A", max.clone()), share("B", max.clone())];
    assert!(matches!(
        allocation::validate_caller_split(&candidates, &shares, &max),
        Err(DomainError::AllocationSplitInvalid(_))
    ));
    assert!(credit::validate_credit_targets(&candidates, &shares).is_ok());
    assert!(matches!(
        allocation::build_allocation_entry(&allocation_input(shares.to_vec())),
        Err(DomainError::AmountOutOfRange(_))
    ));
}

#[test]
fn final_cash_and_remaining_share_bounds_are_enforced() {
    let huge = eur("1000000000000000000000000000");
    assert!(matches!(
        build_settlement_entry(&settle(huge.clone(), eur("0.01"))),
        Err(DomainError::AmountOutOfRange(_))
    ));
    assert!(matches!(
        settlement_return::build_settlement_return_entry(&return_input(huge.clone()), &eur("0.01")),
        Err(DomainError::AmountOutOfRange(_))
    ));
    let candidates = [candidate("A", eur("0.01")), candidate("B", huge.clone())];
    assert!(matches!(
        precedence::oldest_first(&candidates, &huge, None),
        Err(DomainError::AmountOutOfRange(_))
    ));
}

#[test]
fn supplied_toward_zero_fee_shares_are_consumed_without_rerounding() {
    // R8e owns computing .03 * .50 / 1.00 toward zero at EUR/2.
    let first =
        settlement_return::build_settlement_return_entry(&return_input(eur("0.50")), &eur("0.01"))
            .unwrap();
    let final_return =
        settlement_return::build_settlement_return_entry(&return_input(eur("0.50")), &eur("0.02"))
            .unwrap();
    assert_eq!(first.lines[1].money, eur("0.49"));
    assert_eq!(final_return.lines[1].money, eur("0.48"));
    assert_eq!(first.lines[2].money, eur("0.01"));
    assert_eq!(final_return.lines[2].money, eur("0.02"));
    balanced(&first);
    balanced(&final_return);
}

#[test]
fn arbitrary_stored_scales_are_preserved_without_registry_reinterpretation() {
    for (code, scale, gross, fee, cash) in [
        ("JPY", 0, "1234", "34", "1200"),
        ("KWD", 3, "12.345", "0.345", "12"),
        ("EUR", 3, "1.001", "0.001", "1"),
    ] {
        let entry =
            build_settlement_entry(&settle(money(gross, code, scale), money(fee, code, scale)))
                .unwrap();
        assert_eq!(entry.lines[0].money, money(cash, code, scale));
        assert!(
            entry
                .lines
                .iter()
                .all(|l| l.money.currency().scale() == scale)
        );
        balanced(&entry);
    }
}
