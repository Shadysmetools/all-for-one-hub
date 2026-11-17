//! The free built-in Windows voice (SAPI, via System.Speech): what One speaks with when
//! ElevenLabs cannot (no credits, 401/402/429, no network).
//!
//! It never plays by itself: each piece is synthesised to a WAV file at the SAME format the
//! premium voice uses (22.05 kHz, 16-bit, mono), the PCM is cut out of it and handed to the same
//! audio channel, so Stop, barge-in and the mute hotkey cut it off exactly like the premium one.
//! The text goes in through an environment variable, never into the script text, so a reply can
//! not inject PowerShell. Nothing is recorded: this only turns text into sound.

use tokio_util::sync::CancellationToken;

/// A piece longer than this is cut: SAPI is only a fallback and sentences are short anyway.
const MAX_CHARS: usize = 4000;
/// One piece must be synthesised within this long.
const PIECE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(25);

/// Picks the voice: Arabic text needs an installed Arabic voice (none = an error, the text stays
/// text-only); anything else prefers a natural en-US voice (Zira), then any en-US, then any English.
const SCRIPT: &str = r#"
$ErrorActionPreference = 'Stop'
Add-Type -AssemblyName System.Speech
$s = New-Object System.Speech.Synthesis.SpeechSynthesizer
$v = @($s.GetInstalledVoices() | Where-Object { $_.Enabled } | ForEach-Object { $_.VoiceInfo })
if ($env:ONE_TTS_LANG -eq 'ar') {
  $pick = $v | Where-Object { $_.Culture.TwoLetterISOLanguageName -eq 'ar' } | Select-Object -First 1
} else {
  $pick = $v | Where-Object { $_.Culture.Name -eq 'en-US' -and $_.Name -match 'Zira|Aria|Jenny' } | Select-Object -First 1
  if (-not $pick) { $pick = $v | Where-Object { $_.Culture.Name -eq 'en-US' } | Select-Object -First 1 }
  if (-not $pick) { $pick = $v | Where-Object { $_.Culture.TwoLetterISOLanguageName -eq 'en' } | Select-Object -First 1 }
}
if (-not $pick) { [Console]::Error.WriteLine('no suitable voice installed'); exit 3 }
if ($env:ONE_TTS_PROBE -eq '1') { exit 0 }
if ($env:ONE_TTS_PROBE -eq '1') { exit 0 }
$s.SelectVoice($pick.Name)
$fmt = New-Object System.Speech.AudioFormat.SpeechAudioFormatInfo(22050, [System.Speech.AudioFormat.AudioBitsPerSample]::Sixteen, [System.Speech.AudioFormat.AudioChannel]::Mono)
$s.SetOutputToWaveFile($env:ONE_TTS_OUT, $fmt)
$s.Speak($env:ONE_TTS_TEXT)
$s.Dispose()
"#;

/// The raw PCM inside a RIFF/WAVE file: the bytes of its `data` chunk (None if it is not WAV).
pub fn wav_pcm(wav: &[u8]) -> Option<&[u8]> {
    if wav.len() < 12 || &wav[0..4] != b"RIFF" || &wav[8..12] != b"WAVE" {
        return None;
    }
    let mut i = 12;
    while i + 8 <= wav.len() {
        let size = u32::from_le_bytes(wav[i + 4..i + 8].try_into().ok()?) as usize;
        let body = i + 8;
        if &wav[i..i + 4] == b"data" {
            // A streamed header may carry a wrong size: never read past the end.
            return Some(&wav[body..body.saturating_add(size).min(wav.len())]);
        }
        i = body.checked_add(size)?.checked_add(size & 1)?;
    }
    None
}

#[cfg(windows)]
fn temp_wav() -> std::path::PathBuf {
    static N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = N.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    std::env::temp_dir().join(format!("one-voice-{}-{n}.wav", std::process::id()))
}

/// Text in, PCM16 (22.05 kHz mono) out. `lang` is the same hint the premium voice gets.
#[cfg(windows)]
pub async fn synth(text: &str, lang: Option<&str>, cancel: &CancellationToken) -> Result<Vec<u8>, String> {
    let text: String = text.chars().take(MAX_CHARS).collect();
    if text.trim().is_empty() {
        return Ok(Vec::new());
    }
    let out = temp_wav();
    let mut cmd = tokio::process::Command::new("powershell.exe");
    cmd.args(["-NoProfile", "-NonInteractive", "-ExecutionPolicy", "Bypass", "-Command", SCRIPT])
        .env("ONE_TTS_TEXT", &text)
        .env("ONE_TTS_OUT", &out)
        .env("ONE_TTS_LANG", lang.unwrap_or("en"))
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true)
        .creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    let child = cmd.spawn().map_err(|_| "the built-in voice could not start".to_string())?;
    let res = tokio::select! {
        _ = cancel.cancelled() => Err("cancelled".to_string()),
        r = tokio::time::timeout(PIECE_TIMEOUT, child.wait_with_output()) => match r {
            Err(_) => Err("the built-in voice timed out".to_string()),
            Ok(Err(_)) => Err("the built-in voice failed".to_string()),
            Ok(Ok(o)) if o.status.success() => Ok(()),
            Ok(Ok(o)) if o.status.code() == Some(3) => Err("no suitable voice installed".to_string()),
            Ok(Ok(_)) => Err("the built-in voice failed".to_string()),
        },
    };
    let bytes = std::fs::read(&out);
    let _ = std::fs::remove_file(&out);
    res?;
    let bytes = bytes.map_err(|_| "the built-in voice made no sound".to_string())?;
    wav_pcm(&bytes).map(<[u8]>::to_vec).filter(|p| !p.is_empty()).ok_or_else(|| "the built-in voice made no sound".to_string())
}

/// Is a voice installed for this language ("ar" or anything else = English)? Asked once per
/// language and remembered: a missing Arabic voice must not spawn PowerShell again on every reply.
#[cfg(windows)]
pub async fn voice_available(lang: Option<&str>) -> bool {
    static SEEN: std::sync::Mutex<Vec<(bool, bool)>> = std::sync::Mutex::new(Vec::new());
    let ar = lang == Some("ar");
    if let Some((_, ok)) = SEEN.lock().unwrap_or_else(|p| p.into_inner()).iter().find(|(a, _)| *a == ar) {
        return *ok;
    }
    let mut cmd = tokio::process::Command::new("powershell.exe");
    cmd.args(["-NoProfile", "-NonInteractive", "-ExecutionPolicy", "Bypass", "-Command", SCRIPT])
        .env("ONE_TTS_PROBE", "1")
        .env("ONE_TTS_LANG", if ar { "ar" } else { "en" })
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true)
        .creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    let Ok(child) = cmd.spawn() else { return false };
    let ok = match tokio::time::timeout(PIECE_TIMEOUT, child.wait_with_output()).await {
        Ok(Ok(o)) if o.status.success() => true,
        Ok(Ok(o)) if o.status.code() == Some(3) => false,
        _ => return false, // could not find out: not remembered, asked again next time
    };
    SEEN.lock().unwrap_or_else(|p| p.into_inner()).push((ar, ok));
    ok
}

#[cfg(not(windows))]
pub async fn voice_available(_lang: Option<&str>) -> bool {
    false
}

#[cfg(not(windows))]
pub async fn synth(_text: &str, _lang: Option<&str>, _cancel: &CancellationToken) -> Result<Vec<u8>, String> {
    Err("the built-in voice exists on Windows only".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wav(chunks: &[(&[u8; 4], Vec<u8>)]) -> Vec<u8> {
        let mut v = b"RIFF\0\0\0\0WAVE".to_vec();
        for (id, body) in chunks {
            v.extend_from_slice(*id);
            v.extend_from_slice(&(body.len() as u32).to_le_bytes());
            v.extend_from_slice(body);
            if body.len() % 2 == 1 {
                v.push(0);
            }
        }
        v
    }

    #[test]
    fn the_pcm_is_cut_out_of_a_wav_whatever_chunks_come_first() {
        let w = wav(&[(b"fmt ", vec![1; 16]), (b"LIST", vec![9; 5]), (b"data", vec![1, 2, 3, 4])]);
        assert_eq!(wav_pcm(&w), Some(&[1u8, 2, 3, 4][..]));
    }

    #[test]
    fn a_lying_or_cut_header_never_reads_past_the_end() {
        let mut w = wav(&[(b"data", vec![7; 4])]);
        w[16..20].copy_from_slice(&u32::MAX.to_le_bytes());
        assert_eq!(wav_pcm(&w), Some(&[7u8; 4][..]));
        assert_eq!(wav_pcm(b"nope"), None);
        assert_eq!(wav_pcm(&wav(&[(b"fmt ", vec![0; 16])])), None);
    }

    #[test]
    fn the_script_never_contains_reply_text() {
        // the text is passed in the environment, so a reply cannot become PowerShell
        assert!(SCRIPT.contains("$env:ONE_TTS_TEXT") && !SCRIPT.contains("{text}"));
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn english_is_installed_and_the_answer_is_remembered() {
        assert!(voice_available(None).await);
        assert!(voice_available(Some("en")).await, "second ask is served from memory");
    }

    /// Real Windows voice: synthesises (does not play) a sentence and checks real PCM came out.
    #[cfg(windows)]
    #[tokio::test]
    async fn the_windows_voice_really_synthesises_english() {
        let pcm = synth("Hello, this is the built-in voice.", None, &CancellationToken::new()).await.expect("SAPI voice");
        assert!(pcm.len() > 22050, "more than half a second of 22.05 kHz 16-bit audio, got {} bytes", pcm.len());
        assert!(pcm.iter().any(|b| *b != 0), "not silence");
    }
}
