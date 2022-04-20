use crate::read_chunk;
use crate::video::Video;
use anyhow::{bail, Result};
use async_std::fs::File;
use futures::Stream;
use futures::StreamExt;
use futures::TryStreamExt;
// use async_std::stream::StreamExt;
// use futures_util::{StreamExt, TryStreamExt};
use reqwest::header;
use reqwest_middleware::{ClientBuilder, ClientWithMiddleware};
use reqwest_retry::policies::ExponentialBackoff;
use reqwest_retry::RetryTransientMiddleware;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::ffi::OsStr;
use std::path::Path;
use std::time::Duration;

pub struct Upos {
    client: ClientWithMiddleware,
    bucket: Bucket,
    url: String,
    upload_id: String,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct Bucket {
    chunk_size: usize,
    auth: String,
    endpoint: String,
    biz_id: usize,
    upos_uri: String,
}

#[derive(Serialize, Deserialize, Debug)]
#[serde(rename_all = "camelCase")]
pub struct Protocol<'a> {
    upload_id: &'a str,
    chunks: usize,
    total: u64,
    chunk: usize,
    size: usize,
    part_number: usize,
    start: u64,
    end: u64,
}

impl Upos {
    pub async fn from(bucket: Bucket) -> Result<Self> {
        let mut headers = header::HeaderMap::new();
        headers.insert("X-Upos-Auth", header::HeaderValue::from_str(&bucket.auth)?);
        let client = reqwest::Client::builder()
            .user_agent("Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 Chrome/63.0.3239.108")
            .default_headers(headers)
            .timeout(Duration::new(300, 0))
            .build()
            .unwrap();
        let retry_policy = ExponentialBackoff::builder().build_with_max_retries(3);
        let client = ClientBuilder::new(client)
            // Retry failed requests.
            .with(RetryTransientMiddleware::new_with_policy(retry_policy))
            .build();
        let url = format!(
            "https:{}/{}",
            bucket.endpoint,
            bucket.upos_uri.replace("upos://", "")
        ); // 视频上传路径
        let upload_id: serde_json::Value = client
            .post(format!("{url}?uploads&output=json"))
            .send()
            .await?
            .json()
            .await?;
        let upload_id = upload_id["upload_id"].as_str().unwrap().into();
        // let ret =  &upload.ret;
        // let chunk_size = ret["chunk_size"].as_u64().unwrap() as usize;
        // let auth = ret["auth"].as_str().unwrap();
        // let endpoint = ret["endpoint"].as_str().unwrap();
        // let biz_id = &ret["biz_id"];
        // let upos_uri = ret["upos_uri"].as_str().unwrap();
        Ok(Upos {
            client,
            bucket,
            url,
            upload_id,
        })
    }

    pub async fn upload_stream(
        &self,
        file: File,
        limit: usize,
    ) -> Result<impl Stream<Item = Result<(Value, usize)>> + '_> {
        // let mut parts = Vec::new();

        let total_size = file.metadata().await?.len();
        // let parts = Vec::new();
        // let parts_cell = &RefCell::new(parts);
        let chunk_size = self.bucket.chunk_size;
        let chunks_num = (total_size as f64 / chunk_size as f64).ceil() as usize; // 获取分块数量
                                                                                  // let file = tokio::io::BufReader::with_capacity(chunk_size, file);
        let client = &self.client;
        let url = &self.url;
        let upload_id = &*self.upload_id;
        let stream = read_chunk(file, chunk_size)
            // let mut chunks = read_chunk(file, chunk_size)
            .enumerate()
            .map(move |(i, chunk)| async move {
                let chunk = chunk?;
                let len = chunk.len();
                // println!("{}", len);
                let params = Protocol {
                    upload_id,
                    chunks: chunks_num,
                    total: total_size,
                    chunk: i,
                    size: len,
                    part_number: i + 1,
                    start: u64::try_from(i).unwrap() * u64::try_from(chunk_size).unwrap(),
                    end: u64::try_from(i).unwrap() * u64::try_from(chunk_size).unwrap() + u64::try_from(len).unwrap(),
                };

                client.put(url).query(&params).body(chunk).send().await?;

                Ok::<_, anyhow::Error>((
                    json!({"partNumber": params.chunk + 1, "eTag": "etag"}),
                    len,
                ))
            })
            .buffer_unordered(limit);
        Ok(stream)
        // tokio::pin!(stream);
        // while let Some((part, size)) = stream.try_next().await? {
        //     parts.push(part);
        //     // yield UploadStatus::Processing(size);
        // }
        // let res = self.get_ret_video_info(&parts, path).await?;
    }

    pub async fn upload(&self, file: File, path: &Path) -> Result<Video> {
        let parts: Vec<_> = self
            .upload_stream(file, 3)
            .await?
            .map(|union| Ok::<_, reqwest_middleware::Error>(union?.0))
            .try_collect()
            .await?;
        // .for_each_concurrent()
        // .try_collect().await?;
        // let mut parts = Vec::with_capacity(chunks_num);
        // .for_each_concurrent()
        // .try_collect().await?;
        // let mut parts = Vec::with_capacity(chunks_num);
        // tokio::pin!(stream);

        // .for_each_concurrent()
        // .try_collect().await?;
        // let mut parts = Vec::with_capacity(chunks_num);
        // .for_each_concurrent()
        // .try_collect().await?;
        // let mut parts = Vec::with_capacity(chunks_num);
        // tokio::pin!(stream);
        // while let Some((part, size)) = stream.try_next().await? {
        //     parts.push(part);
        //     // (callback)(instant, total_size, size);
        //     // if !callback(instant, total_size, size) {
        //     //     bail!("移除视频");
        //     // }
        // }
        // println!(
        //     "{:.2} MB/s.",
        //     total_size as f64 / 1000. / instant.elapsed().as_millis() as f64
        // );
        self.get_ret_video_info(&parts, path).await
    }

    pub(crate) async fn get_ret_video_info(&self, parts: &[Value], path: &Path) -> Result<Video> {
        // println!("{:?}", parts_cell.borrow());
        let value = json!({
            "name": path.file_name().and_then(OsStr::to_str),
            "uploadId": self.upload_id,
            "biz_id": self.bucket.biz_id,
            "output": "json",
            "profile": "ugcupos/bup"
        });
        // let res: serde_json::Value = self.client.post(url).query(&value).json(&json!({"parts": *parts_cell.borrow()}))
        let res: serde_json::Value = self
            .client
            .post(&self.url)
            .query(&value)
            .json(&json!({ "parts": parts }))
            .send()
            .await?
            .json()
            .await?;
        if res["OK"] != 1 {
            bail!("{}", res)
        }
        Ok(Video {
            title: path
                .file_stem()
                .and_then(OsStr::to_str)
                .map(|s| s.to_string()),
            filename: Path::new(&self.bucket.upos_uri)
                .file_stem()
                .unwrap()
                .to_str()
                .unwrap()
                .into(),
            desc: "".into(),
        })
    }
}
