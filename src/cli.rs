use anyhow::{anyhow, Context, Result};

use biliup::client::{Client, LoginInfo};
use biliup::line::Probe;
use biliup::video::{BiliBili, Studio, Video};
use biliup::{line, load_config, VideoFile};
use bytes::{Buf, Bytes};
use clap::{CommandFactory, Parser, Subcommand};
use dialoguer::theme::ColorfulTheme;
use dialoguer::Input;
use dialoguer::Select;
use futures::{Stream, StreamExt};
use image::Luma;
use indicatif::{ProgressBar, ProgressStyle};
use qrcode::render::unicode;
use qrcode::QrCode;
use reqwest::Body;
use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::pin::Pin;

use std::task::Poll;
use std::time::Instant;

#[derive(Parser)]
#[clap(author, version, about)]
struct Cli {
    /// Turn debugging information on
    // #[clap(short, long, parse(from_occurrences))]
    // debug: usize,

    #[clap(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// 登录B站并保存登录信息在执行目录下
    Login,
    /// 上传视频
    Upload {
        // Optional name to operate on
        // name: Option<String>,
        /// 需要上传的视频路径,若指定配置文件投稿不需要此参数
        #[clap(parse(from_os_str))]
        video_path: Vec<PathBuf>,

        /// Sets a custom config file
        #[clap(short, long, parse(from_os_str), value_name = "FILE")]
        config: Option<PathBuf>,

        /// 选择上传线路，支持kodo, bda2, qn, ws
        #[clap(short, long)]
        line: Option<String>,

        /// 单视频文件最大并发数
        #[clap(long, default_value = "3")]
        limit: usize,

        #[clap(flatten)]
        studio: Studio,
    },
    /// 追加视频
    Append {
        // Optional name to operate on
        // name: Option<String>,
        /// 是否要对某稿件追加视频，avid为稿件 av 号
        #[clap(short, long)]
        avid: u64,
        /// 需要上传的视频路径,若指定配置文件投稿不需要此参数
        #[clap(parse(from_os_str))]
        video_path: Vec<PathBuf>,

        /// 选择上传线路，支持kodo, bda2, qn, ws
        #[clap(short, long)]
        line: Option<String>,

        /// 单视频文件最大并发数
        #[clap(long, default_value = "3")]
        limit: usize,

        #[clap(flatten)]
        studio: Studio,
    },
}

pub async fn parse() -> Result<()> {
    let cli = Cli::parse();

    // You can check the value provided by positional arguments, or option arguments
    // if let Some(name) = cli.name.as_deref() {
    //     println!("Value for name: {}", name);
    // }
    //
    // if let Some(config_path) = cli.config.as_deref() {
    //     println!("Value for config: {}", config_path.display());
    // }

    // You can see how many times a particular flag or argument occurred
    // Note, only flags can have multiple occurrences
    // match cli.debug {
    //     0 => println!("Debug mode is off"),
    //     1 => println!("Debug mode is kind of on"),
    //     2 => println!("Debug mode is on"),
    //     _ => println!("Don't be crazy"),
    // }

    // You can check for the existence of subcommands, and if found use their
    // matches just as you would the top level app
    let client: Client = Default::default();
    match cli.command {
        Commands::Login => {
            login(client).await?;
        }
        Commands::Upload {
            video_path,
            config: None,
            line,
            limit,
            mut studio,
        } if !video_path.is_empty() => {
            println!("number of concurrent futures: {limit}");
            let login_info = client
                .login_by_cookies(std::fs::File::open("cookies.json")?)
                .await?;
            if studio.title.is_empty() {
                studio.title = video_path[0]
                    .file_stem()
                    .and_then(OsStr::to_str)
                    .map(|s| s.to_string())
                    .unwrap();
            }
            cover_up(&mut studio, &login_info, &client).await?;
            studio.videos = upload(&video_path, &client, line.as_deref(), limit).await?;
            studio.submit(&login_info).await?;
        }
        Commands::Upload {
            video_path: _,
            config: Some(config),
            ..
        } => {
            let login_info = client
                .login_by_cookies(std::fs::File::open("cookies.json")?)
                .await?;
            let config = load_config(&config)?;
            println!("number of concurrent futures: {}", config.limit);
            for (filename_patterns, mut studio) in config.streamers {
                let mut paths = Vec::new();
                for entry in glob::glob(&filename_patterns)?.filter_map(Result::ok) {
                    paths.push(entry);
                }
                if paths.is_empty() {
                    println!("未搜索到匹配的视频文件：{filename_patterns}");
                    continue;
                }
                cover_up(&mut studio, &login_info, &client).await?;
                studio.videos =
                    upload(&paths, &client, config.line.as_deref(), config.limit).await?;
                studio.submit(&login_info).await?;
            }
        }
        Commands::Append {
            video_path,
            avid,
            line,
            limit,
            mut studio,
        } if !video_path.is_empty() => {
            println!("number of concurrent futures: {limit}");
            let login_info = client
                .login_by_cookies(std::fs::File::open("cookies.json")?)
                .await?;
            if studio.title.is_empty() {
                studio.title = video_path[0]
                    .file_stem()
                    .and_then(OsStr::to_str)
                    .map(|s| s.to_string())
                    .unwrap();
            }
            studio.aid = Option::from(avid);
            let mut uploaded_videos = upload(&video_path, &client, line.as_deref(), limit).await?;
            // 更改为 通过 studio 发送请求
            studio.video_data(&login_info).await?;
            studio.videos.append(&mut uploaded_videos);
            studio.edit(&login_info).await?;
        }
        _ => {
            println!("参数不正确请参阅帮助");
            Cli::command().print_help()?
        }
    };
    Ok(())
}

async fn login(client: Client) -> Result<()> {
    let selection = Select::with_theme(&ColorfulTheme::default())
        .with_prompt("选择一种登录方式")
        .default(1)
        .item("账号密码")
        .item("短信登录")
        .item("扫码登录")
        .item("浏览器登录")
        .interact()?;
    let info = match selection {
        0 => login_by_password(client).await?,
        1 => login_by_sms(client).await?,
        2 => login_by_qrcode(client).await?,
        3 => login_by_browser(client).await?,
        _ => panic!(),
    };
    let file = std::fs::File::create("cookies.json")?;
    serde_json::to_writer_pretty(&file, &info)?;
    println!("登录成功，数据保存在{:?}", file);
    Ok(())
}

async fn cover_up(studio: &mut Studio, login_info: &LoginInfo, client: &Client) -> Result<()> {
    if !studio.cover.is_empty() {
        let url = BiliBili::new(login_info, client)
            .cover_up(
                &std::fs::read(Path::new(&studio.cover))
                    .with_context(|| format!("cover: {}", studio.cover))?,
            )
            .await?;
        println!("{url}");
        studio.cover = url;
    }
    Ok(())
}

pub async fn upload(
    video_path: &[PathBuf],
    client: &Client,
    line: Option<&str>,
    limit: usize,
) -> Result<Vec<Video>> {
    let mut videos = Vec::new();
    let line = match line {
        Some("kodo") => line::kodo(),
        Some("bda2") => line::bda2(),
        Some("ws") => line::ws(),
        Some("qn") => line::qn(),
        Some("cos") => line::cos(),
        Some("cos-internal") => line::cos_internal(),
        Some(name) => panic!("不正确的线路{name}"),
        None => Probe::probe().await.unwrap_or_default(),
    };
    // let line = line::kodo();
    for video_path in video_path {
        println!("{line:?}");
        let video_file = VideoFile::new(video_path)?;
        let total_size = video_file.total_size;
        let file_name = video_file.file_name.clone();
        let uploader = line.to_uploader(video_file);
        //Progress bar
        let pb = ProgressBar::new(total_size);
        pb.set_style(ProgressStyle::default_bar()
            .template("{spinner:.green} [{elapsed_precise}] [{wide_bar:.cyan/blue}] {bytes}/{total_bytes} ({bytes_per_sec}, {eta})")?);
        // pb.enable_steady_tick(Duration::from_secs(1));
        // pb.tick()

        let instant = Instant::now();

        let video = uploader
            .upload(client, limit, |vs| {
                vs.map(|chunk| {
                    let pb = pb.clone();
                    let (chunk, len) = chunk?;

                    Ok((Progressbar::new(chunk, pb), len))
                })
            })
            .await?;
        pb.finish_and_clear();
        let t = instant.elapsed().as_millis();
        println!(
            "Upload completed: {file_name} => cost {:.2}s, {:.2} MB/s.",
            t as f64 / 1000.,
            total_size as f64 / 1000. / t as f64
        );
        videos.push(video);
    }
    Ok(videos)
}

pub async fn login_by_password(client: Client) -> Result<LoginInfo> {
    let username: String = Input::with_theme(&ColorfulTheme::default())
        .with_prompt("请输入账号")
        .interact()?;
    let password: String = Input::with_theme(&ColorfulTheme::default())
        .with_prompt("请输入密码")
        .interact()?;
    client.login_by_password(&username, &password).await
}

pub async fn login_by_sms(client: Client) -> Result<LoginInfo> {
    let country_code: u32 = Input::with_theme(&ColorfulTheme::default())
        .with_prompt("请输入手机国家代码")
        .default(86)
        .interact_text()?;
    let phone: u64 = Input::with_theme(&ColorfulTheme::default())
        .with_prompt("请输入手机号")
        .interact_text()?;
    let res = client.send_sms(phone, country_code).await?;
    let input: u32 = Input::with_theme(&ColorfulTheme::default())
        .with_prompt("请输入验证码")
        .interact_text()?;
    // println!("{}", payload);
    client.login_by_sms(input, res).await
}

pub async fn login_by_qrcode(client: Client) -> Result<LoginInfo> {
    let value = client.get_qrcode().await?;
    let code = QrCode::new(value["data"]["url"].as_str().unwrap()).unwrap();
    let image = code
        .render::<unicode::Dense1x2>()
        .dark_color(unicode::Dense1x2::Light)
        .light_color(unicode::Dense1x2::Dark)
        .build();
    println!("{}", image);
    // Render the bits into an image.
    let image = code.render::<Luma<u8>>().build();
    println!("在Windows下建议使用Windows Terminal(支持utf8，可完整显示二维码)\n否则可能无法正常显示，此时请打开./qrcode.png扫码");
    // Save the image.
    image.save("qrcode.png").unwrap();
    client.login_by_qrcode(value).await
}

pub async fn login_by_browser(client: Client) -> Result<LoginInfo> {
    let value = client.get_qrcode().await?;
    println!(
        "{}",
        value["data"]["url"].as_str().ok_or(anyhow!("{}", value))?
    );
    println!("请复制此链接至浏览器中完成登录");
    client.login_by_qrcode(value).await
}

#[derive(Clone)]
struct Progressbar {
    bytes: Bytes,
    pb: ProgressBar,
}

impl Progressbar {
    pub fn new(bytes: Bytes, pb: ProgressBar) -> Self {
        Self { bytes, pb }
    }

    pub fn progress(&mut self) -> Result<Option<Bytes>> {
        let pb = &self.pb;

        let content_bytes = &mut self.bytes;

        let n = content_bytes.remaining();

        let pc = 4096;
        if n == 0 {
            Ok(None)
        } else if n < pc {
            pb.inc(n as u64);
            Ok(Some(content_bytes.copy_to_bytes(n)))
        } else {
            pb.inc(pc as u64);

            Ok(Some(content_bytes.copy_to_bytes(pc)))
        }
    }
}

impl Stream for Progressbar {
    type Item = Result<Bytes>;

    fn poll_next(
        mut self: Pin<&mut Self>,
        _cx: &mut std::task::Context<'_>,
    ) -> Poll<Option<Self::Item>> {
        match self.progress()? {
            None => Poll::Ready(None),
            Some(s) => Poll::Ready(Some(Ok(s))),
        }
    }
}

impl From<Progressbar> for Body {
    fn from(async_stream: Progressbar) -> Self {
        Body::wrap_stream(async_stream)
    }
}
