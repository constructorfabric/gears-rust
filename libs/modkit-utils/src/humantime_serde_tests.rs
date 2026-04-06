use serde::{Deserialize, Serialize};

#[test]
fn with() {
    #[derive(Serialize, Deserialize)]
    struct Foo {
        #[serde(with = "super")]
        time: super::Duration,
    }

    let json = r#"{"time": "15 seconds"}"#;
    let foo = serde_json::from_str::<Foo>(json).unwrap();
    assert_eq!(foo.time, super::Duration::from_secs(15));
    let reverse = serde_json::to_string(&foo).unwrap();
    assert_eq!(reverse, r#"{"time":"15s"}"#);
}

#[test]
fn with_option() {
    #[derive(Serialize, Deserialize)]
    struct Foo {
        #[serde(with = "super::option", default)]
        time: Option<super::Duration>,
    }

    let json = r#"{"time": "15 seconds"}"#;
    let foo = serde_json::from_str::<Foo>(json).unwrap();
    assert_eq!(foo.time, Some(super::Duration::from_secs(15)));
    let reverse = serde_json::to_string(&foo).unwrap();
    assert_eq!(reverse, r#"{"time":"15s"}"#);

    let json = r#"{"time": null}"#;
    let foo = serde_json::from_str::<Foo>(json).unwrap();
    assert_eq!(foo.time, None);
    let reverse = serde_json::to_string(&foo).unwrap();
    assert_eq!(reverse, r#"{"time":null}"#);

    let json = r"{}";
    let foo = serde_json::from_str::<Foo>(json).unwrap();
    assert_eq!(foo.time, None);
}

#[test]
fn time() {
    #[derive(Serialize, Deserialize)]
    struct Foo {
        #[serde(with = "super")]
        duration: super::Duration,
    }

    let json = r#"{"duration": "10m 10s"}"#;
    let foo = serde_json::from_str::<Foo>(json).unwrap();
    assert_eq!(foo.duration, super::Duration::new(610, 0));
    let reverse = serde_json::to_string(&foo).unwrap();
    assert_eq!(reverse, r#"{"duration":"10m 10s"}"#);
}

#[test]
fn time_with_option() {
    #[derive(Serialize, Deserialize)]
    struct Foo {
        #[serde(with = "super::option", default)]
        duration: Option<super::Duration>,
    }

    let json = r#"{"duration": "5m"}"#;
    let foo = serde_json::from_str::<Foo>(json).unwrap();
    assert_eq!(foo.duration, Some(super::Duration::new(300, 0)));
    let reverse = serde_json::to_string(&foo).unwrap();
    assert_eq!(reverse, r#"{"duration":"5m"}"#);

    let json = r#"{"duration": null}"#;
    let foo = serde_json::from_str::<Foo>(json).unwrap();
    assert_eq!(foo.duration, None);
    let reverse = serde_json::to_string(&foo).unwrap();
    assert_eq!(reverse, r#"{"duration":null}"#);

    let json = r"{}";
    let foo = serde_json::from_str::<Foo>(json).unwrap();
    assert_eq!(foo.duration, None);
}

#[test]
fn test_option_module() {
    #[derive(Serialize, Deserialize)]
    struct Foo {
        #[serde(with = "super::option")]
        duration: Option<super::Duration>,
    }

    let json = r#"{"duration": "1m"}"#;
    let foo = serde_json::from_str::<Foo>(json).unwrap();
    assert_eq!(foo.duration, Some(super::Duration::from_secs(60)));
    let reverse = serde_json::to_string(&foo).unwrap();
    assert_eq!(reverse, r#"{"duration":"1m"}"#);

    let json = r#"{"duration": null}"#;
    let foo = serde_json::from_str::<Foo>(json).unwrap();
    assert_eq!(foo.duration, None);
    let reverse = serde_json::to_string(&foo).unwrap();
    assert_eq!(reverse, r#"{"duration":null}"#);
}

#[test]
fn test_expecting_message() {
    #[derive(Serialize, Deserialize, Debug)]
    struct Foo {
        #[serde(with = "super")]
        duration: super::Duration,
    }

    let json = r#"{"duration": 123}"#;
    let err = serde_json::from_str::<Foo>(json).unwrap_err();
    assert!(err.to_string().contains("expected a duration"));
}

#[test]
fn test_invalid_string() {
    #[derive(Serialize, Deserialize, Debug)]
    struct Foo {
        #[serde(with = "super")]
        duration: super::Duration,
    }

    let json = r#"{"duration": "not a duration"}"#;
    let err = serde_json::from_str::<Foo>(json).unwrap_err();
    assert!(err.to_string().contains("expected a duration"));
}
