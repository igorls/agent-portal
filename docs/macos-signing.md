# macOS signing and notarization

Agent Portal is distributed outside the Mac App Store. Release builds therefore
use a **Developer ID Application** certificate, the hardened runtime and Apple's
notary service. The bundle identifier is `com.agentportal.desktop`.

Do not use an Apple Development, Apple Distribution or Mac App Distribution
certificate for this workflow. Apple rejects those identities during
notarization.

## Apple account setup

1. The Apple Developer Program Account Holder must accept any pending Program
   License Agreement.
2. Create a **Developer ID Application** certificate for the release team.
   Xcode can do this under **Settings → Accounts → Manage Certificates**, or the
   Account Holder can create it in Certificates, Identifiers & Profiles using a
   certificate signing request from the release Mac.
3. Install the certificate and private key in the login keychain. Confirm that
   it is usable:

   ```sh
   security find-identity -v -p codesigning | grep 'Developer ID Application'
   ```

4. Create an app-specific password for the Apple ID used by the notary service.
   Never put the password, private key or exported certificate in this
   repository.

## Local release build

Set the signing identity and one of Tauri's supported notarization credential
sets. The Apple ID flow is the simplest for a local build:

```sh
export APPLE_SIGNING_IDENTITY='Developer ID Application: Organization Name (TEAMID)'
export APPLE_ID='developer@example.com'
export APPLE_PASSWORD='app-specific-password'
export APPLE_TEAM_ID='TEAMID'

pnpm tauri build --bundles app,dmg
```

Tauri signs with the hardened runtime, uploads the build with `notarytool`, waits
for Apple's result and staples the ticket. The first submission can take longer;
`--skip-stapling` is available for that initial diagnostic pass, but release
artifacts should be built again without it.

Verify the finished bundle before publishing:

```sh
codesign --verify --deep --strict --verbose=2 \
  'target/release/bundle/macos/Agent Portal.app'
spctl --assess --type execute --verbose=4 \
  'target/release/bundle/macos/Agent Portal.app'
xcrun stapler validate \
  'target/release/bundle/macos/Agent Portal.app'
```

## GitHub Actions secrets

Export the Developer ID certificate and its private key from Keychain Access as
a password-protected `.p12`, then base64-encode it:

```sh
openssl base64 -A -in developer-id-application.p12 \
  -out developer-id-application.base64.txt
```

Configure these repository Actions secrets:

- `APPLE_CERTIFICATE`: contents of `developer-id-application.base64.txt`
- `APPLE_CERTIFICATE_PASSWORD`: password used when exporting the `.p12`
- `KEYCHAIN_PASSWORD`: a new random password used only for the ephemeral CI
  keychain
- `APPLE_ID`: Apple ID used for notarization
- `APPLE_PASSWORD`: app-specific password for that Apple ID
- `APPLE_TEAM_ID`: paid Developer Program team ID

The macOS release job imports the certificate into a temporary keychain,
discovers its Developer ID signing identity and lets Tauri sign, notarize and
staple the universal app and DMG. Linux and Windows builds do not receive these
Apple credentials.
