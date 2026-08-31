use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::Path;
use std::sync::Mutex;
use wanax_core::redact;

pub struct RedactingFile(Mutex<File>);

impl RedactingFile {
    pub fn create(path: &Path) -> std::io::Result<Self> {
        if let Some(p) = path.parent() {
            std::fs::create_dir_all(p)?;
        }
        let f = OpenOptions::new().create(true).append(true).open(path)?;
        Ok(Self(Mutex::new(f)))
    }
}

impl Write for RedactingFile {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let text = String::from_utf8_lossy(buf);
        let redacted = redact(&text);
        self.0
            .lock()
            .map_err(|_| std::io::Error::other("log lock"))?
            .write_all(redacted.as_bytes())?;
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.0
            .lock()
            .map_err(|_| std::io::Error::other("log lock"))?
            .flush()
    }
}

pub fn init_run_log(path: &Path) -> Result<(), std::io::Error> {
    let file = RedactingFile::create(path)?;
    let subscriber = tracing_subscriber::fmt()
        .with_ansi(false)
        .with_writer(Mutex::new(file))
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .finish();
    let _ = tracing::subscriber::set_global_default(subscriber);
    Ok(())
}
