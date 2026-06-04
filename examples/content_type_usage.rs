use zjhttpc::requestx::Request;
use zjhttpc::content_type;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Example 1: Using content-type constants
    let request1 = Request::new("POST", "https://api.example.com/data")?
        .set_content_type(content_type::APPLICATION_JSON)
        .set_body_string(r#"{"key": "value"}"#);

    println!("Request 1 - Content-Type: {:?}", request1.content_type);

    // Example 6: Using various content-type constants
    println!("\nAvailable content-type constants:");
    println!("JSON: {}", content_type::APPLICATION_JSON);
    println!("Plain Text: {}", content_type::TEXT_PLAIN);
    println!("HTML: {}", content_type::TEXT_HTML);
    println!("CSS: {}", content_type::TEXT_CSS);
    println!("JavaScript: {}", content_type::TEXT_JAVASCRIPT);
    println!("XML: {}", content_type::APPLICATION_XML);
    println!("Form Data: {}", content_type::APPLICATION_X_WWW_FORM_URLENCODED);
    println!("Multipart Form: {}", content_type::MULTIPART_FORM_DATA);
    println!("PDF: {}", content_type::APPLICATION_PDF);
    println!("PNG: {}", content_type::IMAGE_PNG);
    println!("JPEG: {}", content_type::IMAGE_JPEG);
    println!("SVG: {}", content_type::IMAGE_SVG_XML);

    Ok(())
}
