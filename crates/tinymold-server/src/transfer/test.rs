//! Unit tests for the body bridges.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use http_body_util::{Full, StreamBody};

use super::*;

fn read_all(reader: &mut dyn Read) -> io::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    reader.read_to_end(&mut bytes)?;
    Ok(bytes)
}

#[tokio::test]
async fn an_upload_feeds_the_whole_body_to_the_operation() {
    let body = Full::new(Bytes::from_static(b"uploaded bytes"));
    let (result, received) = upload(body, 1024, read_all).await;

    assert_eq!(result.unwrap(), b"uploaded bytes");
    assert_eq!(received, 14);
}

#[tokio::test]
async fn an_upload_over_the_limit_is_refused_and_the_operation_sees_an_error() {
    let body = Full::new(Bytes::from(vec![0_u8; 100]));
    let (result, _) = upload(body, 10, read_all).await;

    assert!(matches!(result, Err(UploadError::TooLarge)));
}

#[tokio::test]
async fn a_failing_body_is_reported_as_a_body_error() {
    let chunks: Vec<Result<Frame<Bytes>, io::Error>> = vec![
        Ok(Frame::data(Bytes::from_static(b"part"))),
        Err(io::Error::other("reset")),
    ];
    let body = StreamBody::new(tokio_stream::iter(chunks));
    let (result, _) = upload(body, 1024, read_all).await;

    assert!(matches!(result, Err(UploadError::Body)));
}

#[tokio::test]
async fn an_operation_error_is_passed_back() {
    let body = Full::new(Bytes::from_static(b"x"));
    let (result, _) = upload(body, 1024, |_| Err::<(), _>("store refused")).await;

    assert!(matches!(result, Err(UploadError::Store("store refused"))));
}

#[tokio::test]
async fn small_and_large_files_download_whole() {
    let dir = tempfile::tempdir().unwrap();
    for len in [10_usize, 1 << 20] {
        let path = dir.path().join(format!("{len}"));
        let bytes: Vec<u8> = (0..len).map(|i| u8::try_from(i % 251).unwrap()).collect();
        std::fs::write(&path, &bytes).unwrap();

        let (body, size) = download(path).await.unwrap();
        assert_eq!(size, len as u64);
        assert_eq!(body.collect().await.unwrap().to_bytes(), bytes);
    }
}

#[tokio::test]
async fn a_missing_file_downloads_as_none() {
    let dir = tempfile::tempdir().unwrap();

    assert!(download(dir.path().join("gone")).await.is_none());
}
