Download **Residue-macos-universal.dmg** below: one app for Apple silicon and Intel Macs, macOS 13 or later. Open it and drag residue to Applications.

The app is not notarized yet, so macOS blocks it the first time you open it. To open it once, and from then on like any other app:

1. Open residue from Applications. macOS says it could not verify the app. Click **Done**.
2. Open **System Settings > Privacy & Security**, scroll down to **Security**, and click **Open Anyway** next to the message about residue. The button is there for about an hour after you tried to open the app.
3. macOS asks again: click **Open**, and enter your login password if it asks.

Control-click > Open no longer skips this step on macOS 15 and later. The [README](https://github.com/whiteheap/residue-desktop#first-launch) has the details.

To check the download, run these in the folder that holds the DMG and `SHA256SUMS`. The second needs the GitHub CLI and shows that this repository's release workflow built the DMG from this tag.

```sh
shasum -a 256 -c SHA256SUMS
gh attestation verify Residue-macos-universal.dmg --repo whiteheap/residue-desktop --signer-workflow whiteheap/residue-desktop/.github/workflows/release.yml --source-ref refs/tags/vX.Y.Z
```
