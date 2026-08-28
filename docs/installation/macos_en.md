# Install ZenClash on macOS

[简体中文](macos.md) · English

This guide applies to macOS packages downloaded from the official ZenClash GitHub Release page.

> [!IMPORTANT]
> The current public macOS package is ad-hoc signed and is not signed with an Apple Developer ID or notarized by Apple. macOS will therefore warn that it cannot verify the developer or check the app for malicious software on first launch. This means Gatekeeper cannot verify the publisher's identity; it does not mean macOS has detected malware.

## System requirements

- An Apple Silicon Mac (M1, M2, M3, M4, or newer)
- macOS 11 Big Sur or newer
- An administrator account for installation and first launch

You can find the chip model under **Apple menu → About This Mac**, or run this command in Terminal:

```sh
uname -m
```

The output must be `arm64`. The current release does not support the `x86_64` architecture used by Intel Macs.

## Download the installer

1. Open [ZenClash Releases](https://github.com/HaiwenZhang/zenclash/releases).
2. Download `ZenClash-<version>-macOS-arm64.dmg` for the current release.
3. Download `SHA256SUMS` from the same release.

Download ZenClash only from its official GitHub repository. A third-party repackaged file may differ from the public build.

## Verify the download

Open Terminal and calculate the DMG's SHA-256 digest. Replace `<version>` with the version you downloaded:

```sh
cd ~/Downloads
shasum -a 256 "ZenClash-<version>-macOS-arm64.dmg"
grep "ZenClash-<version>-macOS-arm64.dmg" SHA256SUMS
```

The 64-character hexadecimal digest printed by both commands must match exactly. If it does not, delete the DMG, download it again from the official release, and do not continue the installation.

## Install ZenClash

1. Double-click the downloaded DMG.
2. Drag `ZenClash.app` onto the `Applications` folder shown in the window.
3. Wait for the copy to finish, then eject the ZenClash disk image in Finder.
4. Open ZenClash from the Applications folder. Do not run it directly from the DMG.

## First launch

Because the current package has not been notarized by Apple, double-clicking it may display one of these warnings:

- “Apple could not verify ZenClash is free of malware that may harm your Mac or compromise your privacy.”
- “ZenClash cannot be opened because the developer cannot be verified.”

Use the per-app override provided by macOS:

1. Click **Done** or **Cancel** in the warning dialog.
2. Open **System Settings → Privacy & Security**.
3. Scroll down to **Security** and find the message that ZenClash was blocked.
4. Click **Open Anyway**. The button is normally available for about one hour after an attempted launch.
5. Authenticate with your login password or Touch ID, then click **Open** again.

macOS saves an exception for that ZenClash build. You may need to repeat these steps after installing a new version whose contents have changed. See Apple's guide to [opening a Mac app from an unknown developer](https://support.apple.com/guide/mac-help/open-a-mac-app-from-an-unknown-developer-mh40616/mac).

### If “Open Anyway” is unavailable

First confirm that the SHA-256 digest matches, then verify the integrity of the ad-hoc signatures inside the app bundle:

```sh
codesign --verify --deep --strict --verbose=2 "/Applications/ZenClash.app"
```

If verification fails, delete the app and DMG and download them again. Do not bypass the failure and run the app.

If signature verification succeeds but **Open Anyway** remains unavailable, remove only ZenClash's download quarantine attribute:

```sh
xattr -dr com.apple.quarantine "/Applications/ZenClash.app"
open -a ZenClash
```

This affects only `/Applications/ZenClash.app`; it does not disable Gatekeeper globally. Do not use commands such as `spctl --master-disable` that globally disable macOS security checks.

## Initial setup and TUN authorization

1. Open ZenClash and go to **Profiles**.
2. Add an online subscription or import a local Clash/Mihomo YAML file.
3. Return to the home page and select a profile and proxy node.
4. Enable the system proxy or TUN when needed.

ZenClash requests macOS administrator authorization only after you explicitly enable TUN. Creating the TUN network interface and routes requires elevated privileges, so a password or Touch ID prompt is expected. Confirm that the request came from the ZenClash copy you just installed before approving it.

## Update

ZenClash only notifies you about a new version and opens the official Release page; it never installs app updates silently. To update:

1. Quit ZenClash, disabling the system proxy and TUN first.
2. Download the new DMG and `SHA256SUMS`, then verify the digest again.
3. Drag the new `ZenClash.app` into Applications and confirm that you want to replace the old version.
4. Follow the first-launch steps above to allow the new build.

Application data is stored in `~/Library/Application Support/ZenClash`. Replacing `/Applications/ZenClash.app` does not remove existing profiles, settings, or local history.

## Troubleshooting

### Network access is unavailable after a forced quit

First reopen ZenClash, disable the system proxy and TUN from the home page, and then quit normally. If ZenClash has already been removed, open **System Settings → Network → current network service → Details → Proxies** and disable the web proxy, secure web proxy, or automatic proxy configuration previously enabled by ZenClash.

### macOS reports that the app is damaged

1. Delete `/Applications/ZenClash.app` and the downloaded DMG.
2. Download the DMG and `SHA256SUMS` again from the official release.
3. Confirm that the SHA-256 digest matches exactly.
4. Reinstall the app and run the `codesign --verify` command shown above.

Do not force the app to run with `xattr` if its digest or signatures fail verification. Instead, open a [GitHub issue](https://github.com/HaiwenZhang/zenclash/issues) with the ZenClash version, macOS version, Mac chip, and complete error message.

### ZenClash does not run on an Intel Mac

The current DMG contains only the Apple Silicon `arm64` build. Intel Macs are not currently supported.

## Uninstall

1. Disable the system proxy and TUN in ZenClash.
2. Disable **Launch at login** in application settings.
3. Quit ZenClash normally.
4. Move `/Applications/ZenClash.app` to the Trash.

These steps retain your profiles, settings, and history. To remove all local data permanently, first confirm that you no longer need any configuration or backup, then run:

```sh
rm -f "$HOME/Library/LaunchAgents/dev.zenclash.app.plist"
rm -rf "$HOME/Library/Application Support/ZenClash"
```

The second command permanently deletes ZenClash profiles, settings, logs, traffic history, and the managed core. It cannot be undone.
