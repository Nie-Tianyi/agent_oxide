mod core;

use futures_util::StreamExt;
use reqwest::Client;
use serde_json::json;
use std::error::Error;
use std::io::{Write, stdout};

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    // 加载 .env 中的环境变量
    dotenvy::dotenv().ok();
    let api_key =
        std::env::var("DeepSeek_API").expect("未找到环境变量 DeepSeek_API，请在 .env 文件中设置");

    let client = Client::new();

    // 构造 DeepSeek stream 请求体
    let body = json!({
        "model": "deepseek-chat",
        "messages": [
            {"role": "user", "content": "用中文简单介绍一下 Rust 语言的特点，控制在 100 字以内。"}
        ],
        "stream": true
    });

    println!("═══════════════════════════════════════════");
    println!("  发送请求体:");
    println!("{}", serde_json::to_string_pretty(&body).unwrap());
    println!("═══════════════════════════════════════════\n");

    // 发起 POST 请求到 DeepSeek API
    let response = client
        .post("https://api.deepseek.com/v1/chat/completions")
        .bearer_auth(&api_key)
        .json(&body)
        .send()
        .await?;

    // 检查状态码
    if !response.status().is_success() {
        let status = response.status();
        let err_body = response.text().await.unwrap_or_default();
        eprintln!("请求失败, 状态码: {status}\n响应体: {err_body}");
        return Ok(());
    }

    // ── 核心：逐块读取原始响应流 ──────────────────────────────────
    let mut stream = response.bytes_stream();
    let mut total_bytes = 0usize;

    println!("═══════════════════════════════════════════");
    println!("  开始接收 DeepSeek 流式响应原始报文");
    println!("═══════════════════════════════════════════\n");

    while let Some(chunk_result) = stream.next().await {
        match chunk_result {
            Ok(chunk) => {
                total_bytes += chunk.len();

                // ── 分隔线 ──
                println!("───────────────────────────────────────────");
                println!(
                    "[块] 大小: {} bytes | 累计: {} bytes",
                    chunk.len(),
                    total_bytes
                );
                println!("───────────────────────────────────────────");

                // 打印原始 UTF-8 文本（SSE 格式）
                let text = String::from_utf8_lossy(&chunk);
                print!("{}", text);
                stdout().flush()?;

                // 如果最后没有换行，补一个
                if !text.ends_with('\n') {
                    println!();
                }
            }
            Err(e) => {
                eprintln!("\n⚠ 读取流时发生网络错误: {e}");
                break;
            }
        }
    }

    println!("\n═══════════════════════════════════════════");
    println!("  流式传输结束，共接收 {total_bytes} 字节");
    println!("═══════════════════════════════════════════");
    Ok(())
}
