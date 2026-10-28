//! A mock SFTP server on an in-memory duplex pipe.

use russh_sftp::client::SftpSession;
use russh_sftp::protocol::{Attrs, File, FileAttributes, Handle, Name, Status, StatusCode};
use russh_sftp::server::Handler;

#[derive(Default)]
struct Mock {
    listed: bool,
}

/// SFTP has no separate type field: the mode bits are the type.
fn attrs(dir: bool) -> FileAttributes {
    FileAttributes {
        permissions: Some(if dir { 0o040755 } else { 0o100644 }),
        ..Default::default()
    }
}

impl Handler for Mock {
    type Error = StatusCode;

    fn unimplemented(&self) -> Self::Error {
        StatusCode::OpUnsupported
    }

    async fn init(
        &mut self,
        version: u32,
        _extensions: std::collections::HashMap<String, String>,
    ) -> Result<russh_sftp::protocol::Version, Self::Error> {
        eprintln!("server: init v{version}");
        Ok(russh_sftp::protocol::Version {
            version: 3,
            extensions: Default::default(),
        })
    }

    async fn open(
        &mut self,
        id: u32,
        filename: String,
        _pflags: russh_sftp::protocol::OpenFlags,
        _attrs: FileAttributes,
    ) -> Result<Handle, Self::Error> {
        eprintln!("server: open {filename}");
        if filename != "/home/user/notes.txt" {
            return Err(StatusCode::NoSuchFile);
        }
        Ok(Handle { id, handle: "notes.txt".to_owned() })
    }

    async fn read(
        &mut self,
        id: u32,
        handle: String,
        offset: u64,
        len: u32,
    ) -> Result<russh_sftp::protocol::Data, Self::Error> {
        const BODY: &[u8] = b"hello from the mock";
        if handle != "notes.txt" {
            return Err(StatusCode::NoSuchFile);
        }
        let start = offset as usize;
        if start >= BODY.len() {
            return Err(StatusCode::Eof);
        }
        let end = (start + len as usize).min(BODY.len());
        Ok(russh_sftp::protocol::Data {
            id,
            data: BODY[start..end].to_vec(),
        })
    }

    async fn close(&mut self, id: u32, handle: String) -> Result<Status, Self::Error> {
        eprintln!("server: close {handle}");
        Ok(Status {
            id,
            status_code: StatusCode::Ok,
            error_message: String::new(),
            language_tag: String::new(),
        })
    }

    async fn realpath(&mut self, id: u32, path: String) -> Result<Name, Self::Error> {
        eprintln!("server: realpath {path} id={id}");
        Ok(Name { id, files: vec![File::new(path, attrs(true))] })
    }

    async fn stat(&mut self, id: u32, path: String) -> Result<Attrs, Self::Error> {
        eprintln!("server: stat {path}");
        Ok(Attrs { id, attrs: attrs(true) })
    }

    async fn opendir(&mut self, id: u32, path: String) -> Result<Handle, Self::Error> {
        eprintln!("server: opendir {path}");
        Ok(Handle { id, handle: "dir".to_owned() })
    }

    async fn readdir(&mut self, id: u32, handle: String) -> Result<Name, Self::Error> {
        eprintln!("server: readdir {handle} listed={}", self.listed);
        if self.listed {
            return Err(StatusCode::Eof);
        }
        self.listed = true;
        Ok(Name {
            id,
            files: vec![File::new("notes.txt", attrs(false)), File::new("src", attrs(true))],
        })
    }
}

#[test]
fn a_listing_arrives_over_a_duplex_pipe() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime");
    runtime.block_on(async {
        let (client, server) = tokio::io::duplex(64 * 1024);
        tokio::spawn(async move {
            russh_sftp::server::run(server, Mock::default()).await;
        });
        eprintln!("client: connecting");
        let session = SftpSession::new(client).await.expect("handshake");
        eprintln!("client: handshaken");
        let dir = session.read_dir("/home/user").await.expect("read_dir");
        let mut names: Vec<String> = dir.map(|entry| entry.file_name()).collect();
        names.sort();
        eprintln!("client: names {names:?}");
        assert_eq!(names, ["notes.txt", "src"]);
    });
}

#[test]
fn our_list_and_download_work_over_the_mock() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime");
    runtime.block_on(async {
        let (client, server) = tokio::io::duplex(64 * 1024);
        tokio::spawn(async move {
            russh_sftp::server::run(server, Mock::default()).await;
        });

        let mut session = dtr_sftp::Session::from_stream(client)
            .await
            .expect("session over a stream");
        let entries = session.list("/home/user").await.expect("list");
        let names: Vec<&str> = entries.iter().map(|entry| entry.name.as_str()).collect();
        // Directories first: the mock returns the file first on purpose.
        assert_eq!(names, ["src", "notes.txt"]);
        assert!(entries[0].dir, "the directory sorts first, and says so");

        let tmp = std::env::temp_dir().join("dtr-sftp-download-test.txt");
        let _ = std::fs::remove_file(&tmp);
        let written = session
            .download("/home/user/notes.txt", &tmp)
            .await
            .expect("download");
        assert_eq!(written, b"hello from the mock".len() as u64);
        assert_eq!(std::fs::read(&tmp).expect("read back"), b"hello from the mock");
        assert!(!tmp.with_extension("part").exists(), "the .part file is renamed away");
        let _ = std::fs::remove_file(&tmp);
    });
}
