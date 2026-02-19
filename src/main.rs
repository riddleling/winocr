use clap::Parser;
use tracing::Level;
use wild;
use infer;
use tower_http::{limit::RequestBodyLimitLayer, trace::{DefaultOnFailure, DefaultOnRequest, DefaultOnResponse, TraceLayer}};
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};
use std::{fs, io::{self, Write}, path::{Path, PathBuf}};
use windows::{
    Foundation::Rect, Graphics::Imaging::BitmapDecoder, Media::Ocr::{OcrEngine, OcrLine}, Storage::{FileAccessMode, StorageFile}, core::HSTRING
};
use axum::{
    extract::{DefaultBodyLimit, Multipart, Request}, 
    http::{HeaderMap, StatusCode}, 
    middleware::{self, Next}, 
    response::{Html, IntoResponse, Response}, 
    routing::{get, post}, 
    Json, 
    Router
};
use base64::{Engine as _, engine::general_purpose};
use termcolor::{Color, ColorChoice, ColorSpec, StandardStream, WriteColor};
use uuid::Uuid;
use serde::Serialize;
use regex::Regex;

// app version
const VERSION: &str = env!("CARGO_PKG_VERSION");
// upload dir name
const UPLOAD_DIR_NAME: &str = "winocr_uploads";

/// An OCR Tool using Windows.Media.Ocr.OcrEngine API
#[derive(Parser, Debug)]
#[command(author, version, about)]
struct Args {
    /// Input files
    #[arg(required(false))]
    files: Vec<String>,

    /// OCR and export text files
    #[arg(short('o'), long, conflicts_with = "server")]
    ocr: bool,

    /// Run HTTP Server
    #[arg(short('s'), long, conflicts_with = "ocr")]
    server: bool,

    /// HTTP Basic Auth (username:password)
    #[arg(short('a'), long, default_value = "")]
    auth: String,

    /// HTTP port number
    #[arg(short, long, default_value_t = 8000)]
    port: u32,
}

// Upload Json Response
#[derive(Serialize)]
struct UploadResponse {
    success: bool,
    message: String,
    ocr_result: String,
    image_width: i32,
    image_height: i32,
    ocr_boxes: Vec<OCRBoxItem>
}

#[derive(Serialize)]
struct OCRBoxItem {
    text: String,
    x: f32,
    y: f32,
    w: f32,
    h: f32
}

impl OCRBoxItem {
    fn new(text: String, x: f32, y: f32, w: f32, h: f32) -> Self {
        OCRBoxItem { text, x, y, w, h }
    }
}

#[derive(Serialize)]
struct OCRResult {
    text: String,
    image_width: i32,
    image_height: i32,
    boxes: Vec<OCRBoxItem>
}

impl OCRResult {
    fn new(text: String, image_width: i32, image_height: i32, boxes: Vec<OCRBoxItem>) -> Self {
        OCRResult {
            text,
            image_width,
            image_height,
            boxes,
        }
    }
}


#[tokio::main]
async fn main() {
    let args_iter = wild::args();
    let args = Args::parse_from(args_iter);

    if !args.ocr && !args.server {
        for file in args.files {
            if is_image(&file) {
                let mut path = std::env::current_dir().unwrap();
                path.push(file.clone());
                
                if let Ok(ocr_result) = get_ocr_result(path) {
                    print!("{}", ocr_result.text);
                }
            }
        }
    } else if args.ocr { 
        for file in args.files {
            if is_image(&file) {
                let mut path = std::env::current_dir().unwrap();
                path.push(file.clone());
                
                if let Ok(ocr_result) = get_ocr_result(path) {
                    if let Some(stem) = Path::new(&file).file_stem().and_then(|s| s.to_str()) {
                        let text_file = format!("{}{}", stem, ".txt");
                        if let Ok(_) = export_text_file(&ocr_result.text, &text_file) {
                            println!("{} --> {}", file, text_file);
                        }
                    }
                }
            }
        }
    }

    if args.server {
        tracing_subscriber::registry()
            .with(
                tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| {
                    format!("{}=debug,tower_http=debug", env!("CARGO_CRATE_NAME")).into()
                }),
            )
            .with(tracing_subscriber::fmt::layer())
            .init();

        let mut stdout = StandardStream::stdout(ColorChoice::Always);

        let upload_dir = std::env::temp_dir().join(UPLOAD_DIR_NAME);
        std::fs::create_dir_all(&upload_dir).unwrap();

        let app = Router::new()
        .route("/", get(show_form))
        .route("/upload", post(upload_file))
        .layer(DefaultBodyLimit::disable())
        .layer(RequestBodyLimitLayer::new(
            100 * 1024 * 1024, /* 100mb */
        ))
        .layer(
            TraceLayer::new_for_http()
                .on_request(
                    DefaultOnRequest::new()
                        .level(Level::INFO)
                )
                .on_response(
                    DefaultOnResponse::new()
                        .level(Level::INFO)
                        .latency_unit(tower_http::LatencyUnit::Millis),
                )
                .on_failure(
                    DefaultOnFailure::new()
                        .level(Level::ERROR)
                )
        );

        let app = if !args.auth.is_empty() && is_valid_auth_format(&args.auth) {
            print!("      Auth: ");
            stdout.set_color(ColorSpec::new().set_fg(Some(Color::Blue)).set_bold(true)).unwrap();
            writeln!(&mut stdout, "{}", args.auth).unwrap();
            stdout.reset().unwrap();

            let (username, password) = args.auth.split_once(':').unwrap();
            let username = username.to_string();
            let password = password.to_string();

            app.layer(middleware::from_fn(move |headers, request, next| {
                basic_auth_middleware_with_params(headers, request, next, username.clone(), password.clone())
            }))
        } else {
            app
        };

        let addr = format!("0.0.0.0:{}", args.port.to_string());

        print!("   Address: ");
        stdout.set_color(ColorSpec::new().set_fg(Some(Color::Blue)).set_bold(true)).unwrap();
        writeln!(&mut stdout, "http://{}", addr).unwrap();
        stdout.reset().unwrap();

        print!("Upload dir: ");
        stdout.set_color(ColorSpec::new().set_fg(Some(Color::Blue)).set_bold(true)).unwrap();
        writeln!(&mut stdout, "{}", upload_dir.to_str().unwrap()).unwrap();
        stdout.reset().unwrap();
        println!("");
                
        let listener = tokio::net::TcpListener::bind(&addr).await.unwrap();
        axum::serve(listener, app).await.unwrap();
    }
}

fn is_image(path: &str) -> bool {
    let data = fs::read(path);
    match data {
        Ok(data) => infer::is_image(&data),
        Err(_) => false
    }
}

fn get_ocr_result(path: PathBuf) -> io::Result<OCRResult> {
    let file =
        StorageFile::GetFileFromPathAsync(&HSTRING::from(path.to_str().unwrap()))?.get()?;
    let stream = file.OpenAsync(FileAccessMode::Read)?.get()?;

    let decode = BitmapDecoder::CreateAsync(&stream)?.get()?;
    let bitmap = decode.GetSoftwareBitmapAsync()?.get()?;

    let width  = bitmap.PixelWidth()?;
    let height = bitmap.PixelHeight()?;

    let engine = OcrEngine::TryCreateFromUserProfileLanguages()?;
    let result = engine.RecognizeAsync(&bitmap)?.get()?;

    let mut items: Vec<OCRBoxItem> = Vec::new();
    let mut result_text = String::new();

    for line in result.Lines()? {
        let text = format!("{}", line.Text()?);
        result_text.push_str(&format!("{}\n", text));
        let rect = calc_line_rect(&line)?;
        items.push(OCRBoxItem::new(text, rect.X, rect.Y, rect.Width, rect.Height));
    }

    let ocr_result = OCRResult::new(
        result_text,
        width,
        height,
        items
    );

    Ok(ocr_result)
}

/// 判斷 Rect 是否為空
fn is_empty_rect(r: &Rect) -> bool {
    r.Width <= 0.0 || r.Height <= 0.0
}

/// 合併兩個 Rect
fn union_rect(a: &Rect, b: &Rect) -> Rect {
    if is_empty_rect(a) {
        return *b;
    }
    if is_empty_rect(b) {
        return *a;
    }

    let left   = a.X.min(b.X);
    let top    = a.Y.min(b.Y);
    let right  = (a.X + a.Width).max(b.X + b.Width);
    let bottom = (a.Y + a.Height).max(b.Y + b.Height);

    Rect {
        X: left,
        Y: top,
        Width: right - left,
        Height: bottom - top,
    }
}

/// 計算整行的 bounding rect
pub fn calc_line_rect(line: &OcrLine) -> windows::core::Result<Rect> {
    let words = line.Words()?;   // IVectorView<OcrWord>

    let mut has_any = false;
    let mut acc = Rect {
        X: 0.0,
        Y: 0.0,
        Width: 0.0,
        Height: 0.0,
    };

    for word in words {
        let r = word.BoundingRect()?;  // 只有 word 有 BoundingRect()

        if !has_any {
            acc = r;
            has_any = true;
        } else {
            acc = union_rect(&acc, &r);
        }
    }

    Ok(expand_rect(acc, 5.0, 5.0))
}

fn expand_rect(r: Rect, pad_x: f32, pad_y: f32) -> Rect {
    Rect {
        X: r.X - pad_x,
        Y: r.Y - pad_y,
        Width: r.Width + pad_x * 2.0,
        Height: r.Height + pad_y * 2.0,
    }
}

fn export_text_file(text: &String, filename: &String) -> io::Result<()> {
    fs::write(filename, text)?;
    Ok(())
}

// Display file upload form
async fn show_form() -> Html<String> {
    let html = format!(
        r#"
        <!doctype html>
        <html>
        <head>
            <meta charset="utf-8">
            <meta name="viewport" content="width=device-width, initial-scale=1.0">
            <title>winocr</title>
        </head>
        <body>
            <h1>winocr v{}</h1>
            <form action="/upload" method="post" enctype="multipart/form-data">
                <label>
                    Choose file: 
                    <input type="file" name="file" required>
                </label>
                <br><br>
                <input type="submit" value="Upload file">
            </form>
        </body>
        </html>
        "#, 
        VERSION
    );
    Html(html)
}

// Handle single file upload – supports HTML and JSON responses
async fn upload_file(headers: HeaderMap, mut multipart: Multipart) -> impl IntoResponse {
    // Determine if the request is an API request (based on the Accept header)
    let is_api_request = headers.get("accept")
        .and_then(|v| v.to_str().ok())
        .map(|accept| accept.contains("application/json"))
        .unwrap_or(false);
    
    // Get the first field
    if let Some(field) = multipart.next_field().await.unwrap() {
        let original_name = field.file_name().unwrap_or("unnamed").to_string();
        let data = field.bytes().await.unwrap();
        
        // Generate a random filename while preserving the original file extension
        let file_extension = std::path::Path::new(&original_name)
            .extension()
            .and_then(|ext| ext.to_str())
            .unwrap_or("");
        
        let random_name = if file_extension.is_empty() {
            Uuid::new_v4().to_string()
        } else {
            format!("{}.{}", Uuid::new_v4(), file_extension)
        };
        
        // Generate a storage path under the system temporary directory
        let upload_dir = std::env::temp_dir().join(UPLOAD_DIR_NAME);
        let save_path = upload_dir.join(&random_name);
        
        // Write to file
        match std::fs::File::create(&save_path) {
            Ok(mut file) => {
                match file.write_all(&data) {
                    Ok(_) => {
                        let mut success = false;
                        let mut title = "❌ The file type is not an image".to_string();
                        let mut message = "The file type is not an image".to_string();
                        let mut ocr_result_text= "".to_string();
                        let mut image_width = 0;
                        let mut image_height = 0;
                        let mut ocr_boxes = Vec::new();

                        if let Some(path_str) = save_path.to_str() {
                            if is_image(&path_str) {
                                if let Ok(ocr_result) = get_ocr_result(save_path) {
                                    ocr_result_text = format!("{}", ocr_result.text);
                                    image_width = ocr_result.image_width;
                                    image_height = ocr_result.image_height;
                                    ocr_boxes = ocr_result.boxes;
                                    message = "File uploaded successfully".to_string();
                                    title = "OCR Result:".to_string();
                                    success = true;
                                }
                            } 
                        } 
                    
                        if is_api_request {
                            Json(UploadResponse {
                                success: success,
                                message: message.to_string(),
                                ocr_result: ocr_result_text,
                                image_width: image_width,
                                image_height: image_height,
                                ocr_boxes: ocr_boxes
                            }).into_response()
                        } else {
                            Html(format!(
                                r#"
                                <!doctype html>
                                <html>
                                <head>
                                    <meta charset="utf-8">
                                    <meta name="viewport" content="width=device-width, initial-scale=1.0">
                                    <title>OCR Result</title>
                                </head>
                                <body>
                                    <h1>{}</h1>
                                    <p>{}</p>
                                </body>
                                </html>
                                "#,
                                title, ocr_result_text.replace("&", "&amp;").replace("<", "&lt;").replace(">", "&gt;").replace("\n", "<br>")
                            )).into_response()
                        }
                    }
                    Err(_) => {
                        if is_api_request {
                            Json(UploadResponse {
                                success: false,
                                message: "Failed to write file".to_string(),
                                ocr_result: "".to_string(),
                                image_width: 0,
                                image_height: 0,
                                ocr_boxes: Vec::new()
                            }).into_response()
                        } else {
                            Html(r#"
                                <!doctype html>
                                <head>
                                    <meta charset="utf-8">
                                    <meta name="viewport" content="width=device-width, initial-scale=1.0">
                                    <title>Error</title>
                                </head>
                                <html><body>
                                    <h1>❌ Failed to write file.</h1>
                                </body></html>
                            "#.to_string()).into_response()
                        }
                    }
                }
            }
            Err(_) => {
                if is_api_request {
                    Json(UploadResponse {
                        success: false,
                        message: "Unable to create file".to_string(),
                        ocr_result: "".to_string(),
                        image_width: 0,
                        image_height: 0,
                        ocr_boxes: Vec::new()
                    }).into_response()
                } else {
                    Html(r#"
                        <!doctype html>
                        <head>
                            <meta charset="utf-8">
                            <meta name="viewport" content="width=device-width, initial-scale=1.0">
                            <title>Error</title>
                        </head>
                        <html><body>
                            <h1>❌ Unable to create file.</h1>
                        </body></html>
                    "#.to_string()).into_response()
                }
            }
        }
    } else {
        if is_api_request {
            Json(UploadResponse {
                success: false,
                message: "No file received".to_string(),
                ocr_result: "".to_string(),
                image_width: 0,
                image_height: 0,
                ocr_boxes: Vec::new()
            }).into_response()
        } else {
            Html(r#"
                <!doctype html>
                <head>
                    <meta charset="utf-8">
                    <meta name="viewport" content="width=device-width, initial-scale=1.0">
                    <title>Error</title>
                </head>
                <html><body>
                    <h1>❌ No file received</h1>
                </body></html>
            "#.to_string()).into_response()
        }
    }
}

fn is_valid_auth_format(input: &str) -> bool {
    let re = Regex::new(r"^[^:]+:[^:]+$").unwrap();
    re.is_match(input)
}

// Basic Auth middleware
async fn basic_auth_middleware_with_params(
    headers: HeaderMap,
    request: Request,
    next: Next,
    username: String,
    password: String,
) -> std::result::Result<Response, StatusCode> {
    if let Some(auth_header) = headers.get("authorization") {
        if let Ok(auth_str) = auth_header.to_str() {
            if auth_str.starts_with("Basic ") {
                let encoded = &auth_str[6..]; // Remove the 'Basic ' prefix
                if let Ok(decoded_bytes) = general_purpose::STANDARD.decode(encoded) {
                    if let Ok(decoded_str) = String::from_utf8(decoded_bytes) {
                        // Split the username and password
                        if let Some((user, pass)) = decoded_str.split_once(':') {
                            if user == username && pass == password {
                                // Authentication successful, proceeding with the request
                                return Ok(next.run(request).await);
                            }
                        }
                    }
                }
            }
        }
    }

    // Authentication failed, return 401 and request authentication
    let mut response = Response::new("Authentication failed: A valid username and password are required.".into());
    *response.status_mut() = StatusCode::UNAUTHORIZED;
    response.headers_mut().insert(
        "WWW-Authenticate",
        "Basic realm=\"Winocr Server\"".parse().unwrap(),
    );
    
    Ok(response)
}