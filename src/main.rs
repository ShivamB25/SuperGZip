use async_compression::tokio::bufread::{
    GzipDecoder as ReaderGzipDecoder, GzipEncoder as ReaderGzipEncoder,
};
use clap::{Parser, Subcommand};
use std::path::Path;
use std::sync::Arc;
use std::time::Instant;
use tokio::fs::{metadata as async_metadata, File as AsyncFile};
use tokio::io::{
    AsyncReadExt, AsyncWriteExt, BufReader as TokioBufReader, BufWriter as TokioBufWriter,
    Error as TokioIOError, Result as TokioIOResult,
};
use tokio::sync::Semaphore;

async fn is_file(path: &Path) -> bool {
    let metadata = async_metadata(path).await;
    if let Ok(metadata) = metadata {
        metadata.is_file()
    } else {
        false
    }
}

async fn gzip(path: &Path, keep_original: bool) -> TokioIOResult<()> {
    // Silently return if the path is not a file or already ends with .gz
    if !is_file(path).await || path.to_string_lossy().ends_with(".gz") {
        return Ok(());
    }

    // Define the buffer for the compressed data, the reader, and the encoder
    let mut buffer = Vec::new();
    let reader = TokioBufReader::new(AsyncFile::open(path).await?);
    let mut encoder = ReaderGzipEncoder::new(reader);

    // Read the compressed data into the buffer
    encoder.read_to_end(&mut buffer).await?;

    // Define the output path and the writer
    let output_path = format!("{}.gz", path.to_string_lossy());
    let mut writer = TokioBufWriter::new(AsyncFile::create(&output_path).await?);

    // Write the compressed data to the output file and shutdown the writer
    writer.write_all(&buffer).await?;
    writer.shutdown().await?;

    // Delete the original file if keep_original is false (default behavior)
    if !keep_original {
        tokio::fs::remove_file(path).await?;
    }

    Ok(())
}

async fn unzip(path: &Path, keep_original: bool) -> TokioIOResult<()> {
    // Silently return if the path is not a file or does not end with .gz
    if !is_file(path).await || !path.to_string_lossy().ends_with(".gz") {
        return Ok(());
    }

    // Define the buffer for the decompressed data, the reader, and the decoder
    let mut buffer = Vec::new();
    let reader = TokioBufReader::new(AsyncFile::open(path).await?);
    let mut decoder = ReaderGzipDecoder::new(reader);

    // Read the decompressed data into the buffer
    decoder.read_to_end(&mut buffer).await?;

    // Define the output path and file, and the writer
    let output_path = path.with_extension("");
    let output_file = tokio::fs::File::create(&output_path).await?;
    let mut writer = TokioBufWriter::new(output_file);

    // Write the decompressed data to the output file and shutdown the writer
    writer.write_all(&buffer).await?;
    writer.shutdown().await?;

    // Delete the original file if keep_original is false (default behavior)
    if !keep_original {
        tokio::fs::remove_file(path).await?;
    }

    Ok(())
}

#[derive(Parser, Debug)]
#[command(name = "super-gunzip", author, about, version)]
struct SuperGunzip {
    #[command(subcommand)]
    commands: Commands,
}

#[derive(Subcommand, Debug)]
enum Commands {
    /// Compresses all files matching the given pattern using the Gzip algorithm.
    /// Appends a .gz extension to the compressed files
    Gzip {
        /// The glob-like pattern to match files against
        #[arg()]
        pattern: String,

        /// Whether to keep the original files after compression. By default, the original files are deleted.
        #[arg(short, long, action = clap::ArgAction::SetTrue)]
        keep_original: bool,

        /// The maximum number of threads to split the compression across (default: 1)
        #[arg(short, long)]
        num_threads: Option<usize>,
    },

    /// Decompresses all files matching the given pattern using the Gzip algorithm.
    /// Removes the .gz extension from the decompressed files
    Unzip {
        /// The glob-like pattern to match files against
        #[arg()]
        pattern: String,

        /// Whether to keep the original gzipped files after decompression. By default, the original files are deleted.
        #[arg(short, long, action = clap::ArgAction::SetTrue)]
        keep_original: bool,

        /// The maximum number of threads to split the decompression across (default: 1)
        #[arg(short, long)]
        num_threads: Option<usize>,
    },
}

#[tokio::main]
async fn main() -> TokioIOResult<()> {
    let mut errors = vec![];
    let args = SuperGunzip::parse();
    let start = Instant::now();

    match args.commands {
        Commands::Gzip {
            pattern,
            keep_original,
            num_threads,
        } => {
            let _max_threads = num_threads.unwrap_or(1);
            let semaphmore = Arc::new(Semaphore::new(_max_threads));
            let paths = glob::glob(&pattern).unwrap();
            let mut handles = Vec::new();
            for path in paths.flatten() {
                let resource_lock = Arc::clone(&semaphmore);
                let handle = tokio::spawn(async move {
                    let _permit = resource_lock.acquire_owned().await.unwrap();
                    let result = gzip(&path, keep_original).await;
                    drop(_permit);
                    result
                });
                handles.push(handle);
            }
            for handle in handles {
                let result = handle.await?;
                if let Err(e) = result {
                    println!("Error: {}", e);
                    errors.push(e);
                }
            }
        }
        Commands::Unzip {
            pattern,
            keep_original,
            num_threads,
        } => {
            let _max_threads = num_threads.unwrap_or(1);
            let semaphmore = Arc::new(Semaphore::new(_max_threads));
            let paths = glob::glob(&pattern).unwrap();
            let mut handles = Vec::new();
            for path in paths.flatten() {
                let resource_lock = Arc::clone(&semaphmore);
                let handle = tokio::spawn(async move {
                    let _permit = resource_lock.acquire_owned().await.unwrap();
                    let result = unzip(&path, keep_original).await;
                    drop(_permit);
                    result
                });
                handles.push(handle);
            }
            for handle in handles {
                let result = handle.await?;
                if let Err(e) = result {
                    println!("Error: {}", e);
                    errors.push(e);
                }
            }
        }
    }

    println!("Elapsed time: {:?} ms", start.elapsed().as_millis());
    if errors.is_empty() {
        println!("Finished without errors.");
        Ok(())
    } else {
        Err(TokioIOError::new(
            tokio::io::ErrorKind::Other,
            format!("{} errors occurred", errors.len()),
        ))
    }
}
