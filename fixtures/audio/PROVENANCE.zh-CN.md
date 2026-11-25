# 测试音频来源与规格说明

[English](PROVENANCE.md) | 简体中文

本目录下的所有音频文件均为 M0 可行性验证专门生成的 100% 合成音频，不包含任何受版权保护或外部商业录音。

## 生成方式

使用 `ffmpeg` 6.1+ 版本的 `lavfi` (Libavfilter) `sine` 正弦合成音频源，并使用 `libmp3lame` 编码器输出标准固定比特率 (CBR) MP3 文件。

生成脚本：[scripts/generate-test-audio.sh](../../scripts/generate-test-audio.sh)

## 轨道清单

| 文件名 | 频率 | 音高 | 时长 | 编码格式 | 比特率 | 采样率 | 声道数 | 验证用途 |
| --- | --- | --- | --- | --- | --- | --- | --- | --- |
| `track1_440hz.mp3` | 440.00 Hz | A4 | 5 秒 | MP3 (libmp3lame) | 128 kbps CBR | 44,100 Hz | 双声道 (2) | 自动切歌切换测试（第 1 首） |
| `track2_554hz.mp3` | 554.37 Hz | C#5 | 5 秒 | MP3 (libmp3lame) | 128 kbps CBR | 44,100 Hz | 双声道 (2) | 自动切歌切换测试（第 2 首） |
| `track3_659hz.mp3` | 659.25 Hz | E5 | 5 秒 | MP3 (libmp3lame) | 128 kbps CBR | 44,100 Hz | 双声道 (2) | 自动切歌切换测试（第 3 首） |
| `track4_880hz.mp3` | 880.00 Hz | A5 | 5 秒 | MP3 (libmp3lame) | 128 kbps CBR | 44,100 Hz | 双声道 (2) | 自动切歌切换测试（第 4 首） |
| `track_long_30min.mp3` | 440.00 Hz | A4 | 1800 秒 (30 分钟) | MP3 (libmp3lame) | 128 kbps CBR | 44,100 Hz | 双声道 (2) | 30 分钟连续播放与 10 分钟锁屏续播耐久度测试 |

## 版权与授权

- **作者**：FFmpeg libavfilter 纯算法合成
- **授权协议**：CC0 1.0 (Public Domain Dedication / 公有领域)
- **可复现性**：可通过 `scripts/generate-test-audio.sh` 随时无损重新生成
