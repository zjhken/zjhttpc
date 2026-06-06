use std::io::Cursor;
use zjhttpc::body::{BodyMultipartForm, detect_mime_type};
use zjhttpc::requestx::Request;

fn main() -> anyhow::Result<()> {
    // Example 1: Simple text fields
    let form1 = BodyMultipartForm::new()
        .add("username", "alice")
        .add("email", "alice@example.com")
        .add("bio", "Hello, world!");

    println!("Example 1 - Text fields:");
    println!("Boundary: {}", form1.boundary());
    println!("Field count: {}\n", form1.len());

    // Example 2: Form with file path (auto-detect filename and content-type)
    let form2 = BodyMultipartForm::new()
        .add("username", "bob")
        .add_file_path("avatar", "/path/to/avatar.jpg")?;

    println!("Example 2 - With file path:");
    println!("Boundary: {}", form2.boundary());
    println!("Field count: {}\n", form2.len());

    // Example 3: Form with custom filename and content-type
    let form3 = BodyMultipartForm::new()
        .add("username", "charlie")
        .add_file_path_with_options(
            "document",
            "/path/to/file",
            Some("custom_name.pdf"),
            Some("application/pdf")
        )?;

    println!("Example 3 - With custom options:");
    println!("Boundary: {}", form3.boundary());
    println!("Field count: {}\n", form3.len());

    // Example 4: Using with Request
    let request = Request::new("POST", "https://example.com/upload")?
        .set_body_multipart_form(
            BodyMultipartForm::new()
                .add("username", "alice")
                .add("description", "Profile picture upload")
                .add_file_path("avatar", "/path/to/avatar.jpg")?
        );

    println!("Example 4 - Request with multipart form:");
    println!("URL: {}", request.url);
    println!("Content-Type: {}", request.content_type.as_deref().unwrap_or("none"));
    println!("Content-Length: {} (0 = unknown, will use chunked encoding)", request.content_length);

    // Example 5: MIME type detection
    println!("\nExample 5 - MIME type detection:");
    println!("avatar.jpg -> {}", detect_mime_type("avatar.jpg"));
    println!("document.pdf -> {}", detect_mime_type("document.pdf"));
    println!("data.json -> {}", detect_mime_type("data.json"));
    println!("image.png -> {}", detect_mime_type("image.png"));
    println!("unknown.xyz -> {}", detect_mime_type("unknown.xyz"));

    // Example 6: Multiple files
    let form6 = BodyMultipartForm::new()
        .add("title", "Photo Gallery")
        .add("description", "My vacation photos")
        .add_file_path("photo1", "/path/to/photo1.jpg")?
        .add_file_path("photo2", "/path/to/photo2.png")?
        .add_file_path("photo3", "/path/to/photo3.gif")?;

    println!("\nExample 6 - Multiple files:");
    println!("Boundary: {}", form6.boundary());
    println!("Field count: {}", form6.len());

    Ok(())
}
