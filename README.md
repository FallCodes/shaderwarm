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
2. **Cache locally.** The wrapper points Mesa, NVIDIA, DXVK and vkd3d-proton at
   `steamapps/shadercache/<appid>`, the same as Proton-CachyOS's
   `PROTON_LOCAL_SHADER_CACHE`, but on any Proton build.
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

## Use

| Command | What it does |
|---|---|
| `shaderwarm-gui` | Desktop UI: a library of your games' Steam covers with cache status, a detail panel per game (hero art, cache composition, actions), replay with progress, cleanup and settings |
| `shaderwarm list` | Games with cache size, video size and status |
| `shaderwarm status <appid>` | Engine, Proton, caches, recordings, last launch |
| `shaderwarm replay <appid>` / `--stale` / `--all` | Compile recordings into the cache |
| `shaderwarm clean --video` | Delete Steam's transcoded video (often GBs) |
| `shaderwarm clean <appid> --cache [--backup] [--recordings]` | Purge one game's compiled caches |
| `shaderwarm restore <appid>` | Put a `--backup` purge back |
| `shaderwarm fetch [--keep-video] [--no-replay]` | Let Steam download its shader caches once, with Shader Pre-Caching off the rest of the time |

### Fetching Steam's caches on demand

Steam only downloads shader caches through its own logged-in client, so `fetch` (also
under Settings in the GUI) runs it briefly with pre-caching on:

1. Closes Steam and turns Shader Pre-Caching on in `config/config.vdf`.
2. Starts Steam minimised and follows `logs/content_log.txt` until its shader downloads settle.
3. Closes Steam, turns pre-caching off again, and starts Steam again if it was open before.
4. Deletes the video Steam downloaded with the shaders and replays the games it updated.

If the fetch is interrupted, the setting is put back the next time shaderwarm runs while
Steam is closed. With Steam open, use `shaderwarm fetch --finish` or the GUI's Finish button.

Settings live in `~/.config/shaderwarm/settings.toml`. Your own self-compile overrides
go in `~/.config/shaderwarm/self-precompile.toml`, in the same format as
`data/self-precompile.toml`.

## Limits

- Recording covers what you have played. The first visit to a new area still
  compiles once; after that, driver and Proton updates no longer cost you anything.
- Pre-warming straight from game files isn't implemented yet. The Unreal IoStore and
  shader library readers exist (`shaderwarm inspect <appid>`), but DX12 needs the
  root signatures Unreal builds at runtime.
- While recording, vkd3d-proton's `VK_EXT_shader_module_identifier` is hidden, because
  together with the recording layer it crashed a UE5 game.

## License

GPL-3.0-or-later. Fossilize is MIT; the bundled Anton font is SIL OFL 1.1 (`assets/fonts/OFL-Anton.txt`).
