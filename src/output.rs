//! Write and read [`SolverLog`] to disk in JSON, optionally gzipped.
//!
//! Gzipped JSON is the recommended default for archival — the columnar
//! [`ProgressTable`] compresses extremely well (repeated monotonic times,
//! dense Option<T> columns).

use crate::SolverLog;
use flate2::{read::GzDecoder, write::GzEncoder, Compression};
use std::fs::File;
use std::io::{BufReader, BufWriter, Read, Write};
use std::path::Path;

pub fn write_json(path: impl AsRef<Path>, log: &SolverLog) -> std::io::Result<()> {
    let f = BufWriter::new(File::create(path)?);
    serde_json::to_writer(f, log).map_err(io_err)
}

pub fn write_json_pretty(path: impl AsRef<Path>, log: &SolverLog) -> std::io::Result<()> {
    let f = BufWriter::new(File::create(path)?);
    serde_json::to_writer_pretty(f, log).map_err(io_err)
}

pub fn write_json_gz(path: impl AsRef<Path>, log: &SolverLog) -> std::io::Result<()> {
    let f = File::create(path)?;
    let mut gz = GzEncoder::new(BufWriter::new(f), Compression::best());
    serde_json::to_writer(&mut gz, log).map_err(io_err)?;
    gz.finish()?.flush()?;
    Ok(())
}

pub fn read_json(path: impl AsRef<Path>) -> std::io::Result<SolverLog> {
    let path = path.as_ref();
    let f = File::open(path)?;
    let reader: Box<dyn Read> = match path.extension().and_then(|s| s.to_str()) {
        Some("gz" | "olog") => Box::new(GzDecoder::new(f)),
        _ => Box::new(BufReader::new(f)),
    };
    serde_json::from_reader(reader).map_err(io_err)
}

fn io_err(e: serde_json::Error) -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::InvalidData, e)
}
