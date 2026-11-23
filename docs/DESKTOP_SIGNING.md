# Desktop code signing

The daemon and desktop binaries are distributed from GitHub Releases. The Tauri
updater signatures are already configured, but the macOS and Windows bundles are
still unsigned, so Gatekeeper and SmartScreen warn on first launch.

## Current state

- Tauri updater: signed with the offline Tauri key; `latest.json` and `.sig`
  assets are published.
- macOS: unsigned `arm64`/`x64` `.app.tar.gz` and `.dmg`; the installer no
  longer removes `com.apple.quarantine`, so Gatekeeper warns.
- Windows: unsigned NSIS `.exe` and MSI; SmartScreen warns.
- CI workflow comments list Apple/Windows secrets, but the build step does not
  yet pass the certificate environment variables to `tauri-action`. Adding
  secrets alone is not enough; the workflow needs the import/signing steps
  below.

## macOS

Requirements:

- Apple Developer Program membership (US$99/year).
- A **Developer ID Application** certificate and private key, exported as a
  password-protected `.p12`.
- App Store Connect API key (preferred) or an Apple ID app-specific password.
- Apple Team ID.

GitHub repository secrets:

```text
APPLE_CERTIFICATE              base64 of the .p12
APPLE_CERTIFICATE_PASSWORD     .p12 export password
APPLE_SIGNING_IDENTITY         e.g. "Developer ID Application: Name (TEAMID)"
APPLE_ID                       Apple account email (or API key issuer)
APPLE_PASSWORD                 app-specific password (or API key)
APPLE_TEAM_ID                  Apple Team ID
KEYCHAIN_PASSWORD              temporary CI keychain password
```

Workflow additions for each macOS build:

1. Decode `APPLE_CERTIFICATE`, create a temporary keychain, import the `.p12`,
   and grant `codesign` access with `security set-key-partition-list`.
2. Export the imported identity to `APPLE_SIGNING_IDENTITY`.
3. Pass `APPLE_ID`, `APPLE_PASSWORD`, and `APPLE_TEAM_ID` to
   `tauri-apps/tauri-action` so Tauri notarizes the app and DMG.
4. Sign the daemon/CLI binary in the daemon release as well; a notarized app
   bundle does not cover a separate Mach-O downloaded from a tarball.
5. Keep the Tauri updater signature separate; it is not an OS code signature.

Verification on a clean Mac:

```bash
codesign --verify --deep --strict --verbose=2 "Orqella Desktop.app"
spctl --assess --type execute --verbose=4 "Orqella Desktop.app"
xcrun stapler validate "Orqella Desktop.app"
xcrun stapler validate Orqella.Desktop_<version>.dmg
```

## Windows

Requirements:

- An OV or EV code-signing certificate from a CA (DigiCert, Sectigo, SSL.com,
  etc.). EV gives immediate SmartScreen reputation; OV builds reputation over
  time. Some CAs require a hardware token or cloud HSM.
- Azure Trusted Signing is a lower-friction alternative if the organization is
  eligible; it uses the `trusted-signing-action` or `signtool` with the Azure
  signing endpoint.

GitHub repository secrets:

```text
WINDOWS_CERTIFICATE           base64 of the .pfx
WINDOWS_CERTIFICATE_PASSWORD  .pfx password
```

Workflow additions:

1. On `windows-2022`, decode and import the certificate into
   `Cert:\CurrentUser\My` with `Import-PfxCertificate`.
2. Let Tauri sign the MSI/NSIS bundles, or invoke `signtool sign /fd SHA256
   /tr <timestamp-url> /td SHA256` on each artifact.
3. Sign the combined `Orqella-Setup-<version>-windows-x86_64.exe` in
   `release.yml`, not only the desktop bundles.
4. Verify with `signtool verify /pa /v <file>` before publishing.

Verification on a clean Windows VM:

- Download from the public release URL and check the SmartScreen prompt.
- Confirm the publisher is `Orqella` and the timestamp is present.
- Install, launch, update through the Tauri updater, and uninstall.

## Release process

- Keep the Tauri updater key and the OS signing keys in separate secret stores.
- Sign the same artifact that is checksummed in `SHA256SUMS.txt`; do not rebuild
  after signing.
- Publish the signed artifact and its `.sig`; update the website warning only
  after both macOS and Windows bundles are signed.
- The macOS installer should continue to leave quarantine in place; a signed
  and notarized app passes Gatekeeper without the `xattr` bypass.

## Cost and lead time

- Apple Developer Program: US$99/year; certificate and notarization setup is
  usually under a day after enrollment approval.
- Windows OV certificate: roughly US$200–400/year, with identity/business
  validation taking days to weeks. EV is more expensive and may require a
  hardware token or HSM.
- Budget a clean-VM verification pass for each platform after the first signed
  release.
