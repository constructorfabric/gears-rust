//! `#[derive(ProtoBridge)]` happy path — struct with `via_string` and direct
//! fields, plus a unit enum. Stubs are stand-ins that mirror the prost
//! shape: enums as `i32` round-trippable via `TryFrom<i32>`.

use toolkit_contract::ProtoBridge;
use uuid::Uuid;

mod stubs {
    #[derive(Debug, Clone, PartialEq, Default)]
    pub struct ChargeRequest {
        pub amount_cents: i64,
        pub currency: String,
    }

    #[derive(Debug, Clone, PartialEq)]
    pub struct ChargeResponse {
        pub payment_id: String,
        pub status: i32,
    }

    #[derive(Debug, Clone, Copy, PartialEq, Default)]
    #[repr(i32)]
    pub enum PaymentStatus {
        #[default]
        Pending = 0,
        Completed = 1,
        Failed = 2,
    }

    impl TryFrom<i32> for PaymentStatus {
        type Error = ();
        fn try_from(v: i32) -> Result<Self, ()> {
            match v {
                0 => Ok(Self::Pending),
                1 => Ok(Self::Completed),
                2 => Ok(Self::Failed),
                _ => Err(()),
            }
        }
    }
}

#[derive(Debug, Clone, ProtoBridge)]
#[proto_bridge(stub = "crate::stubs::ChargeRequest")]
pub struct ChargeRequest {
    pub amount_cents: i64,
    pub currency: String,
}

#[derive(Debug, Clone, ProtoBridge)]
#[proto_bridge(stub = "crate::stubs::ChargeResponse")]
pub struct ChargeResponse {
    #[proto_bridge(via_string)]
    pub payment_id: Uuid,
    pub status: PaymentStatus,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, ProtoBridge)]
#[proto_bridge(stub = "crate::stubs::PaymentStatus")]
pub enum PaymentStatus {
    #[default]
    Pending,
    Completed,
    Failed,
}

fn main() {
    let req = ChargeRequest {
        amount_cents: 100,
        currency: "USD".into(),
    };
    let proto: stubs::ChargeRequest = req.clone().into();
    assert_eq!(proto.amount_cents, 100);
    assert_eq!(proto.currency, "USD");
    let back: ChargeRequest = proto.into();
    assert_eq!(back.amount_cents, 100);

    let status_i32: i32 = PaymentStatus::Completed.into();
    assert_eq!(status_i32, 1);
    let status_back: PaymentStatus = 99i32.into();
    assert_eq!(status_back, PaymentStatus::Pending);
}
