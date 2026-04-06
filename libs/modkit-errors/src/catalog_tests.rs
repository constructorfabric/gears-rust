use super::*;

#[test]
fn err_def_to_problem_works() {
    use http::StatusCode;

    let def = ErrDef {
        status: StatusCode::NOT_FOUND.as_u16(),
        title: "Not Found",
        code: "TEST_NOT_FOUND",
        type_url: "https://errors.example.com/TEST_NOT_FOUND",
    };

    let problem = def.as_problem("Resource missing");
    assert_eq!(problem.status, StatusCode::NOT_FOUND);
    assert_eq!(problem.title, "Not Found");
    assert_eq!(problem.detail, "Resource missing");
    assert_eq!(problem.code, "TEST_NOT_FOUND");
    assert_eq!(
        problem.type_url,
        "https://errors.example.com/TEST_NOT_FOUND"
    );
}
