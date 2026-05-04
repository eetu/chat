#[actix_web::main]
async fn main() -> std::io::Result<()> {
    chat_backend::run_server().await
}
