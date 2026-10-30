# shaderwarm

Pre-compiles shaders for Proton games on Linux, so they don't stutter while compiling
shaders mid-game. It is built for CachyOS and AMD (RADV); Intel (ANV) and NVIDIA use
the same code path but are untested.

It replaces Steam's *Shader Pre-Caching*: no shader or video downloads, just the
pipelines your games actually use, kept in each game's own cache and rebuilt
automatically after GPU driver or Proton updates.

## How it works

1. **Record.** Launch games through `shaderwarm run`. A Vulkan layer
   ([Fossilize](https://github.com/ValveSoftware/Fossilize)) records every pipeline the
   game builds, whether it comes from DXVK (DX9-11), vkd3d-proton (DX12) or native
   Vulkan. Each launch records only pipelines that are new.
2. **Cache locally.** Each game uses its own cache in `steamapps/shadercache/<appid>`.
   Current Steam clients already point Mesa, NVIDIA, DXVK and vkd3d-proton there, even
   with pre-caching off. The wrapper fills in any of those variables that are missing,
   like Proton-CachyOS's `PROTON_LOCAL_SHADER_CACHE` but on any Proton build, so you
   don't need to turn that option on.
3. **Replay.** `fossilize-replay` compiles the recordings into that cache at idle
   priority. Steam's own `.foz` recordings from earlier pre-caching are used too.
4. **Stay fresh.** Each game's cache is fingerprinted by the GPU's
   `pipelineCacheUUID`, the Proton build and the recordings. A systemd user service
   replays stale games after driver or Proton updates, and never while a game is
   running.

Games that compile their own shaders up front are flagged (curated list plus a check
for their own PSO caches). Unreal Engine 5 games are never flagged, because UE5's
precaching misses most pipelines.

## Install

```sh
# Arch / CachyOS package (builds Fossilize too)
cd packaging && makepkg -fsi   # -f: rebuild even if an older package file exists

# or from source
sudo pacman -S --needed rustup cmake ninja
./scripts/build-fossilize.sh
cargo build --release
ln -s "$PWD/target/release/shaderwarm" "$PWD/target/release/shaderwarm-gui" ~/.local/bin/
./scripts/install-desktop.sh   # app-menu entry and icon (undo: --uninstall)
```

Then:

```sh
shaderwarm setup     # recording layer + auto-replay units (undo: setup --uninstall)
shaderwarm doctor    # GPU, driver, Fossilize, Steam settings
```

In Steam, turn off *Settings > Downloads > Shader Pre-Caching* and set each game's
launch option to:

```
shaderwarm run -- %command%
```

Other wrappers go after it, for example
`shaderwarm run -- mangohud %command%`. With goverlay's `bgmod`, use
`shaderwarm run -- "/path/to/bgmod" %command%`. Its custom-variables field only
sets variables and cannot run commands.

Variables can go before `shaderwarm` or straight after `--`; both of these work:

```
PROTON_ENABLE_WAYLAND=1 shaderwarm run -- %command%
shaderwarm run -- PROTON_ENABLE_WAYLAND=1 %command%
```

`--` marks where shaderwarm's options end and the game's command begins.

## Use

| Command | What it does |
|---|---|
| `shaderwarm-gui` | Desktop UI: a library of your games' Steam covers with cache status, a detail panel per game (hero art, cache composition, actions), replay with progress, freeing disk space (Steam video and caches of uninstalled games), Fetch from Steam and settings |
| `shaderwarm list` | Games with cache size, video size and status |
| `shaderwarm status <appid>` | Engine, Proton, caches, recordings, last launch |
| `shaderwarm replay <appid>` / `--stale` / `--all` | Compile recordings into the cache |
| `shaderwarm clean --video` | Delete Steam's transcoded video (often GBs) |
| `shaderwarm clean --orphans` | Delete caches and recordings of games that are no longer installed |
| `shaderwarm clean <appid> --cache [--backup] [--recordings]` | Purge one game's compiled caches |
| `shaderwarm restore <appid>` | Put a `--backup` purge back |
| `shaderwarm fetch [--keep-video] [--no-replay]` | Let Steam download its shader caches once, with Shader Pre-Caching off the rest of the time |
| `shaderwarm fetch --finish` | Turn pre-caching back off after an interrupted fetch |

### Fetching Steam's caches on demand

Steam only downloads shader caches through its own logged-in client, so `fetch` (also
under Settings in the GUI) runs it briefly with pre-caching on:

1. Closes Steam and turns Shader Pre-Caching on in `config/config.vdf`.
2. Starts Steam minimised and follows `logs/content_log.txt` until its shader downloads settle.
3. Closes Steam, turns pre-caching off again, and starts Steam again if it was open before.
4. Deletes the video Steam downloaded with the shaders and replays the games it updated.

If the fetch is interrupted, the setting is put back the next time shaderwarm runs while
Steam is closed. With Steam open, use `shaderwarm fetch --finish` or the GUI's Finish button.

### Checking that the cache works

RADV can print a running count of pipeline cache hits and misses. Add the variables to a
game's launch option:

```
RADV_DEBUG=psocachestats PROTON_LOG=1 shaderwarm run -- %command%
```

Proton writes the log to `~/steam-<appid>.log`; Proton-CachyOS adds its build to the
name (`~/steam-<appid>-<build>.log`). While playing, show only the moments the driver
had to compile:

```sh
tail -F ~/steam-<appid>*.log | grep --line-buffered -E "misses=[1-9]"
```

If a stutter happens with no line appearing, it isn't shader compilation. One-off
hitches the first time something appears in a launch are often the game loading
assets, which no shader cache can fix. Remove the variables afterwards: Proton's log
costs performance and grows quickly.

Settings live in `~/.config/shaderwarm/settings.toml`. Your own self-compile overrides
go in `~/.config/shaderwarm/self-precompile.toml`, in the same format as
`data/self-precompile.toml`.

## Limits

- Recording covers what you have played. The first visit to a new area still
  compiles once; after that, driver and Proton updates no longer cost you anything.
- Pre-warming straight from game files isn't implemented yet. The Unreal IoStore and
  shader library readers exist (`shaderwarm inspect <appid>`), but DX12 needs the
  root signatures Unreal builds at runtime.
- Pipelines depend on the DXVK and vkd3d-proton versions in each Proton build. After
  switching a game to another build (GE, CachyOS), older recordings only
  match where those versions are the same; the rest compiles once in play and is
  recorded from then on. Switching back costs nothing.
- Recording only costs CPU when the game creates a pipeline that isn't recorded yet,
  never per frame. Memory use grows with the number of recorded pipelines.
- While recording, vkd3d-proton's `VK_EXT_shader_module_identifier` is hidden, because
  together with the recording layer it crashed some UE5 games.

## License

GPL-3.0-or-later. Fossilize is MIT; the bundled Anton font is SIL OFL 1.1 (`assets/fonts/OFL-Anton.txt`).
