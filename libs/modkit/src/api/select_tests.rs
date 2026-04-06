use super::*;

#[test]
fn test_project_json_object() {
    let value = json!({
        "id": "123",
        "name": "John",
        "email": "john@example.com",
        "age": 30
    });

    let selected = ["id".to_owned(), "name".to_owned()];
    let fields_set: HashSet<String> = selected.iter().map(|f| f.to_lowercase()).collect();

    let projected = project_json(&value, &fields_set);

    assert_eq!(projected.get("id").and_then(|v| v.as_str()), Some("123"));
    assert_eq!(projected.get("name").and_then(|v| v.as_str()), Some("John"));
    assert!(projected.get("email").is_none());
    assert!(projected.get("age").is_none());
}

#[test]
fn test_project_json_case_insensitive() {
    let value = json!({
        "Id": "123",
        "Name": "John"
    });

    let selected = ["id".to_owned(), "name".to_owned()];
    let fields_set: HashSet<String> = selected.iter().map(|f| f.to_lowercase()).collect();

    let projected = project_json(&value, &fields_set);

    assert_eq!(projected.get("Id").and_then(|v| v.as_str()), Some("123"));
    assert_eq!(projected.get("Name").and_then(|v| v.as_str()), Some("John"));
}

#[test]
fn test_project_json_array() {
    let value = json!([
        {"id": "1", "name": "John"},
        {"id": "2", "name": "Jane"}
    ]);

    let selected = ["id".to_owned()];
    let fields_set: HashSet<String> = selected.iter().map(|f| f.to_lowercase()).collect();

    let projected = project_json(&value, &fields_set);

    let arr = projected.as_array().unwrap();
    assert_eq!(arr.len(), 2);
    assert_eq!(arr[0].get("id").and_then(|v| v.as_str()), Some("1"));
    assert!(arr[0].get("name").is_none());
}

#[test]
fn test_project_json_nested() {
    let value = json!({
        "id": "123",
        "user": {
            "name": "John",
            "email": "john@example.com"
        }
    });

    let selected = ["id".to_owned(), "user".to_owned()];
    let fields_set: HashSet<String> = selected.iter().map(|f| f.to_lowercase()).collect();

    let projected = project_json(&value, &fields_set);

    assert_eq!(projected.get("id").and_then(|v| v.as_str()), Some("123"));
    assert!(projected.get("user").is_some());
}

#[test]
fn test_apply_select_with_fields() {
    #[derive(serde::Serialize)]
    struct User {
        id: String,
        name: String,
        email: String,
    }

    let user = User {
        id: "123".to_owned(),
        name: "John".to_owned(),
        email: "john@example.com".to_owned(),
    };

    let selected = vec!["id".to_owned(), "name".to_owned()];
    let result = apply_select(&user, Some(&selected));

    assert_eq!(result.get("id").and_then(|v| v.as_str()), Some("123"));
    assert_eq!(result.get("name").and_then(|v| v.as_str()), Some("John"));
    assert!(result.get("email").is_none());
}

#[test]
fn test_apply_select_without_fields() {
    #[derive(serde::Serialize)]
    struct User {
        id: String,
        name: String,
    }

    let user = User {
        id: "123".to_owned(),
        name: "John".to_owned(),
    };

    let result = apply_select(&user, None);

    assert_eq!(result.get("id").and_then(|v| v.as_str()), Some("123"));
    assert_eq!(result.get("name").and_then(|v| v.as_str()), Some("John"));
}
