# Drydock

Drydock is a native desktop app for browsing, downloading, and managing Steam games on Windows
and Linux. It also includes tools for game activation, fixes, and language settings.

## Download

**[Get the latest release](https://github.com/NhMarco/Drydock/releases/latest)** or download your app directly:

| System | Processor | Download |
| --- | --- | --- |
| Windows | Intel / AMD 64-bit | [Drydock-windows-x64.exe](https://github.com/NhMarco/Drydock/releases/latest/download/Drydock-windows-x64.exe) |
| Windows | ARM64 | [Drydock-windows-arm64.exe](https://github.com/NhMarco/Drydock/releases/latest/download/Drydock-windows-arm64.exe) |
| Linux | Intel / AMD 64-bit | [Drydock-linux-x64](https://github.com/NhMarco/Drydock/releases/latest/download/Drydock-linux-x64) |
| Linux | ARM64 | [Drydock-linux-arm64](https://github.com/NhMarco/Drydock/releases/latest/download/Drydock-linux-arm64) |

Most Intel and AMD PCs use **x64**. Choose **ARM64** for an ARM device.
On the release page, these files are under **Assets**. The **Source code** archives are for developers.
No installer or Rust installation is needed to use the app.

### Windows

Double-click the downloaded `.exe` and approve the administrator prompt.

If Windows shows **"Windows protected your PC"**, select **More info**, then **Run anyway**. This
appears because the app is not code-signed.

### Linux

Open a terminal in the folder containing the download, make it executable, and run it:

```bash
chmod +x Drydock-linux-x64
./Drydock-linux-x64
```

For ARM64, use `Drydock-linux-arm64` in both commands.

Drydock checks for app updates on startup and asks before installing them. You can also download
a newer version from the [releases page](https://github.com/NhMarco/Drydock/releases).

<details>
<summary>Verify a download (optional)</summary>

Each app has a matching `.sha256` checksum file under the release's **Assets**. Download both
files to the same folder.

On Windows, run this in PowerShell and compare the hash with the one in the `.sha256` file:

```powershell
Get-FileHash .\Drydock-windows-x64.exe -Algorithm SHA256
```

On Linux, put both files in the same folder and run:

```bash
sha256sum -c Drydock-linux-x64.sha256
```

Use the corresponding filenames for ARM64.

</details>

## First launch

1. Sign in with Discord in your browser if prompted. Drydock does not ask for your Steam password.
2. In **Settings**, check the **Steam folder**. If it was not detected, choose it with **Browse**,
   then select **Save and Reload**.
3. Set a **Games folder** for new downloads and save it, or leave it empty to use Steam's library.
4. Browse **Store** or search by game name or Steam App ID. Open a game to see its available actions.

To use **Add to Steam**, install the **Steam Service** from Settings first. The in-app **Help**
page walks through activation and fixes. These features depend on the game's available files;
the emulator tools use Windows binaries.

On Linux, **Add to Steam** is not available: Steam on Linux does not load the Steam Service, so
download games in Drydock instead.

## What you can do

- **Store:** browse featured games, new releases and Denuvo titles as a grid or a list.
- **Add to Steam or download:** choose where a game comes from (DepotBox, SteamTools or Ryu) and see
  how up to date each source is. With your own Hubcap Manifest API key (in **Settings**), Hubcap is a
  choice too.
- **Library:** find installed games, launch them, and manage updates, verification, and removal.
- **Downloads:** queue games, pause and resume transfers, and repair missing or damaged files.
- **Activation and fixes:** generate activation requests, enter response codes, apply available game
  fixes, or get the version a fix is made for.
- **Tools:** change supported game languages and generate Steam emulator configurations.

## Help and app data

If the server cannot be reached, **Continue Offline** lets you access installed games while online
features are unavailable. For bugs, [open an issue](https://github.com/NhMarco/Drydock/issues) with
your app version, operating system, and steps to reproduce the problem. Please report security issues
privately instead ([how](SECURITY.md)).

Settings and app data are stored in `%LOCALAPPDATA%\Drydock` on Windows and
`$XDG_DATA_HOME/Drydock` (usually `~/.local/share/Drydock`) on Linux. Crash reports are saved
as `crash.log` in that folder.

## For developers

Building from source, configuration, running your own proxy and publishing releases are covered in
the [developer guide](docs/DEVELOPMENT.md).

Licensed under [GPL-2.0-only](LICENSE).
