use zjhttpc::body::BodyForm;
use zjhttpc::requestx::Request;

fn main() -> anyhow::Result<()> {
    // Example 1: Simple form data
    let form1 = BodyForm::new()
        .add("username", "alice")
        .add("password", "secret123")
        .add("remember", "true");

    println!("Example 1 - Simple form:");
    println!("Serialized: {}", form1.serialize());
    println!("Length: {}\n", form1.len());

    // Example 2: Form with duplicate keys (multi-select, tags, etc.)
    let form2 = BodyForm::new()
        .add("tags", "rust")
        .add("tags", "http")
        .add("tags", "async");

    println!("Example 2 - Duplicate keys:");
    println!("Serialized: {}", form2.serialize());
    println!("Length: {}\n", form2.len());

    // Example 3: Form with special characters
    let form3 = BodyForm::new()
        .add("email", "user@example.com")
        .add("message", "Hello, world!")
        .add("path", "/api/v1/users");

    println!("Example 3 - Special characters:");
    println!("Serialized: {}", form3.serialize());
    println!("Length: {}\n", form3.len());

    // Example 4: Using with Request
    let request = Request::new("POST", "https://example.com/login")?
        .set_body_form(
            BodyForm::new()
                .add("username", "alice")
                .add("password", "secret")
        );

    println!("Example 4 - Request with form body:");
    println!("URL: {}", request.url);
    println!("Content-Type: {}", request.content_type);
    println!("Content-Length: {}", request.content_length);

    Ok(())
}
