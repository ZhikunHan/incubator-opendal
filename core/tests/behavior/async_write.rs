// Licensed to the Apache Software Foundation (ASF) under one
// or more contributor license agreements.  See the NOTICE file
// distributed with this work for additional information
// regarding copyright ownership.  The ASF licenses this file
// to you under the Apache License, Version 2.0 (the
// "License"); you may not use this file except in compliance
// with the License.  You may obtain a copy of the License at
//
//   http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing,
// software distributed under the License is distributed on an
// "AS IS" BASIS, WITHOUT WARRANTIES OR CONDITIONS OF ANY
// KIND, either express or implied.  See the License for the
// specific language governing permissions and limitations
// under the License.

use anyhow::Result;
use bytes::Buf;
use bytes::Bytes;
use futures::io::BufReader;
use futures::io::Cursor;
use futures::stream;
use futures::StreamExt;
use log::warn;
use sha2::Digest;
use sha2::Sha256;

use crate::*;

pub fn tests(op: &Operator, tests: &mut Vec<Trial>) {
    let cap = op.info().full_capability();

    if cap.write && cap.stat {
        tests.extend(async_trials!(
            op,
            test_write_only,
            test_write_with_empty_content,
            test_write_with_dir_path,
            test_write_with_special_chars,
            test_write_with_cache_control,
            test_write_with_content_type,
            test_write_with_content_disposition,
            test_writer_write,
            test_writer_sink,
            test_writer_copy,
            test_writer_abort,
            test_writer_futures_copy
        ))
    }

    if cap.write && cap.write_can_append && cap.stat {
        tests.extend(async_trials!(
            op,
            test_write_with_append,
            test_writer_with_append
        ))
    }
}

/// Write a single file and test with stat.
pub async fn test_write_only(op: Operator) -> Result<()> {
    let (path, content, size) = TEST_FIXTURE.new_file(op.clone());

    op.write(&path, content).await?;

    let meta = op.stat(&path).await.expect("stat must succeed");
    assert_eq!(meta.content_length(), size as u64);

    Ok(())
}

/// Write a file with empty content.
pub async fn test_write_with_empty_content(op: Operator) -> Result<()> {
    if !op.info().full_capability().write_can_empty {
        return Ok(());
    }

    let path = TEST_FIXTURE.new_file_path();

    op.write(&path, vec![]).await?;

    let meta = op.stat(&path).await.expect("stat must succeed");
    assert_eq!(meta.content_length(), 0);
    Ok(())
}

/// Write file with dir path should return an error
pub async fn test_write_with_dir_path(op: Operator) -> Result<()> {
    let path = TEST_FIXTURE.new_dir_path();

    let result = op.write(&path, vec![1]).await;
    assert!(result.is_err());
    assert_eq!(result.unwrap_err().kind(), ErrorKind::IsADirectory);

    Ok(())
}

/// Write a single file with special chars should succeed.
pub async fn test_write_with_special_chars(op: Operator) -> Result<()> {
    // Ignore test for supabase until https://github.com/apache/incubator-opendal/issues/2194 addressed.
    if op.info().scheme() == opendal::Scheme::Supabase {
        warn!("ignore test for supabase until https://github.com/apache/incubator-opendal/issues/2194 is resolved");
        return Ok(());
    }
    // Ignore test for atomicserver until https://github.com/atomicdata-dev/atomic-server/issues/663 addressed.
    if op.info().scheme() == opendal::Scheme::Atomicserver {
        warn!("ignore test for atomicserver until https://github.com/atomicdata-dev/atomic-server/issues/663 is resolved");
        return Ok(());
    }

    let path = format!("{} !@#$%^&()_+-=;',.txt", uuid::Uuid::new_v4());
    let (path, content, size) = TEST_FIXTURE.new_file_with_path(op.clone(), &path);

    op.write(&path, content).await?;

    let meta = op.stat(&path).await.expect("stat must succeed");
    assert_eq!(meta.content_length(), size as u64);

    Ok(())
}

/// Write a single file with cache control should succeed.
pub async fn test_write_with_cache_control(op: Operator) -> Result<()> {
    if !op.info().full_capability().write_with_cache_control {
        return Ok(());
    }

    let path = uuid::Uuid::new_v4().to_string();
    let (content, _) = gen_bytes(op.info().full_capability());

    let target_cache_control = "no-cache, no-store, max-age=300";
    op.write_with(&path, content)
        .cache_control(target_cache_control)
        .await?;

    let meta = op.stat(&path).await.expect("stat must succeed");
    assert_eq!(meta.mode(), EntryMode::FILE);
    assert_eq!(
        meta.cache_control().expect("cache control must exist"),
        target_cache_control
    );

    op.delete(&path).await.expect("delete must succeed");

    Ok(())
}

/// Write a single file with content type should succeed.
pub async fn test_write_with_content_type(op: Operator) -> Result<()> {
    if !op.info().full_capability().write_with_content_type {
        return Ok(());
    }

    let (path, content, size) = TEST_FIXTURE.new_file(op.clone());

    let target_content_type = "application/json";
    op.write_with(&path, content)
        .content_type(target_content_type)
        .await?;

    let meta = op.stat(&path).await.expect("stat must succeed");
    assert_eq!(meta.mode(), EntryMode::FILE);
    assert_eq!(
        meta.content_type().expect("content type must exist"),
        target_content_type
    );
    assert_eq!(meta.content_length(), size as u64);

    Ok(())
}

/// Write a single file with content disposition should succeed.
pub async fn test_write_with_content_disposition(op: Operator) -> Result<()> {
    if !op.info().full_capability().write_with_content_disposition {
        return Ok(());
    }

    let (path, content, size) = TEST_FIXTURE.new_file(op.clone());

    let target_content_disposition = "attachment; filename=\"filename.jpg\"";
    op.write_with(&path, content)
        .content_disposition(target_content_disposition)
        .await?;

    let meta = op.stat(&path).await.expect("stat must succeed");
    assert_eq!(meta.mode(), EntryMode::FILE);
    assert_eq!(
        meta.content_disposition().expect("content type must exist"),
        target_content_disposition
    );
    assert_eq!(meta.content_length(), size as u64);

    Ok(())
}

/// Delete existing file should succeed.
pub async fn test_writer_abort(op: Operator) -> Result<()> {
    let (path, content, _) = TEST_FIXTURE.new_file(op.clone());

    let mut writer = match op.writer(&path).await {
        Ok(writer) => writer,
        Err(e) => {
            assert_eq!(e.kind(), ErrorKind::Unsupported);
            return Ok(());
        }
    };

    if let Err(e) = writer.write(content).await {
        assert_eq!(e.kind(), ErrorKind::Unsupported);
        return Ok(());
    }

    if let Err(e) = writer.abort().await {
        assert_eq!(e.kind(), ErrorKind::Unsupported);
        return Ok(());
    }

    // Aborted writer should not write actual file.
    assert!(!op.is_exist(&path).await?);
    Ok(())
}

/// Append data into writer
pub async fn test_writer_write(op: Operator) -> Result<()> {
    if !(op.info().full_capability().write_can_multi) {
        return Ok(());
    }

    let path = TEST_FIXTURE.new_file_path();
    let size = 5 * 1024 * 1024; // write file with 5 MiB
    let content_a = gen_fixed_bytes(size);
    let content_b = gen_fixed_bytes(size);

    let mut w = op.writer(&path).await?;
    w.write(content_a.clone()).await?;
    w.write(content_b.clone()).await?;
    w.close().await?;

    let meta = op.stat(&path).await.expect("stat must succeed");
    assert_eq!(meta.content_length(), (size * 2) as u64);

    let bs = op.read(&path).await?;
    assert_eq!(bs.len(), size * 2, "read size");
    assert_eq!(
        format!("{:x}", Sha256::digest(&bs[..size])),
        format!("{:x}", Sha256::digest(content_a)),
        "read content a"
    );
    assert_eq!(
        format!("{:x}", Sha256::digest(&bs[size..])),
        format!("{:x}", Sha256::digest(content_b)),
        "read content b"
    );

    Ok(())
}

/// Streaming data into writer
pub async fn test_writer_sink(op: Operator) -> Result<()> {
    let cap = op.info().full_capability();
    if !(cap.write && cap.write_can_multi) {
        return Ok(());
    }

    let path = TEST_FIXTURE.new_file_path();
    let size = 5 * 1024 * 1024; // write file with 5 MiB
    let content_a = gen_fixed_bytes(size);
    let content_b = gen_fixed_bytes(size);
    let stream = stream::iter(vec![content_a.clone(), content_b.clone()]).map(Ok);

    let mut w = op.writer_with(&path).buffer(5 * 1024 * 1024).await?;
    w.sink(stream).await?;
    w.close().await?;

    let meta = op.stat(&path).await.expect("stat must succeed");
    assert_eq!(meta.content_length(), (size * 2) as u64);

    let bs = op.read(&path).await?;
    assert_eq!(bs.len(), size * 2, "read size");
    assert_eq!(
        format!("{:x}", Sha256::digest(&bs[..size])),
        format!("{:x}", Sha256::digest(content_a)),
        "read content a"
    );
    assert_eq!(
        format!("{:x}", Sha256::digest(&bs[size..])),
        format!("{:x}", Sha256::digest(content_b)),
        "read content b"
    );

    Ok(())
}

/// Reading data into writer
pub async fn test_writer_copy(op: Operator) -> Result<()> {
    let cap = op.info().full_capability();
    if !(cap.write && cap.write_can_multi) {
        return Ok(());
    }

    let path = TEST_FIXTURE.new_file_path();
    let size = 5 * 1024 * 1024; // write file with 5 MiB
    let content_a = gen_fixed_bytes(size);
    let content_b = gen_fixed_bytes(size);

    let mut w = op.writer_with(&path).buffer(5 * 1024 * 1024).await?;

    let mut content = Bytes::from([content_a.clone(), content_b.clone()].concat());
    while !content.is_empty() {
        let reader = Cursor::new(content.clone());
        let n = w.copy(reader).await?;
        content.advance(n as usize);
    }
    w.close().await?;

    let meta = op.stat(&path).await.expect("stat must succeed");
    assert_eq!(meta.content_length(), (size * 2) as u64);

    let bs = op.read(&path).await?;
    assert_eq!(bs.len(), size * 2, "read size");
    assert_eq!(
        format!("{:x}", Sha256::digest(&bs[..size])),
        format!("{:x}", Sha256::digest(content_a)),
        "read content a"
    );
    assert_eq!(
        format!("{:x}", Sha256::digest(&bs[size..])),
        format!("{:x}", Sha256::digest(content_b)),
        "read content b"
    );

    Ok(())
}

/// Copy data from reader to writer
pub async fn test_writer_futures_copy(op: Operator) -> Result<()> {
    if !(op.info().full_capability().write_can_multi) {
        return Ok(());
    }

    let path = TEST_FIXTURE.new_file_path();
    let (content, size): (Vec<u8>, usize) =
        gen_bytes_with_range(10 * 1024 * 1024..20 * 1024 * 1024);

    let mut w = op.writer_with(&path).buffer(8 * 1024 * 1024).await?;

    // Wrap a buf reader here to make sure content is read in 1MiB chunks.
    let mut cursor = BufReader::with_capacity(1024 * 1024, Cursor::new(content.clone()));
    futures::io::copy_buf(&mut cursor, &mut w).await?;
    w.close().await?;

    let meta = op.stat(&path).await.expect("stat must succeed");
    assert_eq!(meta.content_length(), size as u64);

    let bs = op.read(&path).await?;
    assert_eq!(bs.len(), size, "read size");
    assert_eq!(
        format!("{:x}", Sha256::digest(&bs[..size])),
        format!("{:x}", Sha256::digest(content)),
        "read content"
    );

    Ok(())
}

/// Test append to a file must success.
pub async fn test_write_with_append(op: Operator) -> Result<()> {
    let path = TEST_FIXTURE.new_file_path();
    let (content_one, size_one) = gen_bytes(op.info().full_capability());
    let (content_two, size_two) = gen_bytes(op.info().full_capability());

    op.write_with(&path, content_one.clone())
        .append(true)
        .await
        .expect("append file first time must success");

    let meta = op.stat(&path).await?;
    assert_eq!(meta.content_length(), size_one as u64);

    op.write_with(&path, content_two.clone())
        .append(true)
        .await
        .expect("append to an existing file must success");

    let bs = op.read(&path).await.expect("read file must success");

    assert_eq!(bs.len(), size_one + size_two);
    assert_eq!(bs[..size_one], content_one);
    assert_eq!(bs[size_one..], content_two);

    Ok(())
}

/// Copy data from reader to writer
pub async fn test_writer_with_append(op: Operator) -> Result<()> {
    let path = uuid::Uuid::new_v4().to_string();
    let (content, size): (Vec<u8>, usize) =
        gen_bytes_with_range(10 * 1024 * 1024..20 * 1024 * 1024);

    let mut a = op.writer_with(&path).append(true).await?;

    // Wrap a buf reader here to make sure content is read in 1MiB chunks.
    let mut cursor = BufReader::with_capacity(1024 * 1024, Cursor::new(content.clone()));
    futures::io::copy_buf(&mut cursor, &mut a).await?;
    a.close().await?;

    let meta = op.stat(&path).await.expect("stat must succeed");
    assert_eq!(meta.content_length(), size as u64);

    let bs = op.read(&path).await?;
    assert_eq!(bs.len(), size, "read size");
    assert_eq!(
        format!("{:x}", Sha256::digest(&bs[..size])),
        format!("{:x}", Sha256::digest(content)),
        "read content"
    );

    op.delete(&path).await.expect("delete must succeed");
    Ok(())
}
