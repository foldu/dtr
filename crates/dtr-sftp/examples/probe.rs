//! Try a real connection and print what happened.
//!
//! `cargo run -p dtr-sftp --example probe -- <alias> [path]`

#[tokio::main(flavor = "current_thread")]
async fn main() -> anyhow::Result<()> {
    let mut args = std::env::args().skip(1);
    let alias = args.next().expect("usage: probe <alias> [path]");
    let path = args.next();
    let mut session = dtr_sftp::Session::connect(&alias).await?;
    let path = match path {
        Some(path) => std::path::PathBuf::from(path),
        None => session.home().await?,
    };
    let entries = session.list(&path.to_string_lossy()).await?;
    for entry in entries.iter().take(10) {
        println!("{}{}", entry.name, if entry.dir { "/" } else { "" });
    }
    println!("{} entries in {}", entries.len(), path.display());
    session.close();
    Ok(())
}
