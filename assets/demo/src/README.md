# Demo video source

`assets/demo.mp4` and `assets/demo.gif`, plus `poster.png` and `demo.srt` in the parent folder, are generated from this folder.
Everything shown is fictional (Northwind Supplies, Alex/Maya/Sam, +1-555-0100) and comes from the demo runner's
`timeline.json` (`python -m allforone.demo`, and `--lang ar` for the Arabic moment).

## What is here
| File | Role |
|---|---|
| `player.html`, `player.css`, `extra.css`, `player.js`, `scenes*.js` | Motion-graphics player. No CDN; system fonts plus bundled Playfair Display. Data-driven from `data/*.json`. |
| `data/timeline.json`, `data/timeline.ar.json` | Copies of the demo runner output (English and `--lang ar`). |
| `data/voiceover.json` | Script segments, captions and timing hints. `voiceover.txt` is a readable copy. |
| `data/scenes.json` | Generated: scene start/length measured from the voiceover audio. The player's clock. |
| `voiceover/*.mp3` | Cached voice clips (about 1 MB). Re-rendering does not need to call the API. |
| `assets/brand/logo.svg` | The one logo path the player loads (copy of `../../../brand/logo.svg`). Replace it to change the mark. |
| `fonts/` | Playfair Display (OFL) for the wordmark, bundled so the player works offline. |
| `gen_voiceover.py` | Synthesizes any missing clips, measures them, writes `data/scenes.json` and `../demo.srt`. |
| `render.mjs` | Records the player, mixes audio, encodes the MP4, GIF and poster. |

## Re-render (clips already cached)
```bash
cd assets/demo/src
npm i playwright            # or set PW_DIR to a folder that has node_modules/playwright
node render.mjs             # records ~85 s in real time, writes ../../demo.mp4, ../../demo.gif, ../poster.png
node render.mjs --shots     # one PNG per scene into out/shots (quick layout check)
```
Needs Node, ffmpeg on PATH, and Microsoft Edge (`channel: 'msedge'`, no browser download).
Playwright's video recorder wants its own ffmpeg build; if it complains, point
`PLAYWRIGHT_BROWSERS_PATH` at a folder containing `ffmpeg-1010/ffmpeg-win64.exe` (a copy of your ffmpeg works).

Swap the logo: drop the new SVG at `assets/brand/logo.svg` and run `node render.mjs`.

## Voiceover
The voiceover was made with **ElevenLabs** through its official REST API
(`POST /v1/text-to-speech/{voice_id}`, model `eleven_multilingual_v2`, stock voice "Bella").
Generated audio is subject to your ElevenLabs plan terms. To change the script or the voice,
edit `data/voiceover.json` and run `ELEVENLABS_API_KEY=... python gen_voiceover.py` (your own key;
it is only sent in the `xi-api-key` header, never written anywhere). Unchanged segments are
read from `voiceover/` and are not re-billed. Then run `node render.mjs`.
The soft background pad is synthesized by ffmpeg (sine tones), no downloaded music.

Set `CACHE_DIR` to use a different clip folder; set `CRF`, `GIF_START`, `GIF_LEN`, `POSTER_AT` to tune the output.
