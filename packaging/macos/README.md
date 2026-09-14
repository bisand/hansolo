# macOS signing and notarisation

The release workflow signs `hansolo` and `HanSolo.app` with a Developer ID
certificate, notarises them with Apple, and staples the tickets to the app and
the disk image, so both open without a Gatekeeper prompt. It does this only when
the repository has the secrets below. Without them it falls back to ad-hoc
signing and users have to right-click → **Open**.

## 1. Developer ID Application certificate

Signing for distribution outside the App Store needs a **Developer ID
Application** certificate. Apple Development and Apple Distribution certificates
won't work. Only the Account Holder of the Apple Developer team can create one.

1. In Xcode → Settings → Accounts → *team* → **Manage Certificates…**, click **+**
   and choose **Developer ID Application**. You can also create it at
   <https://developer.apple.com/account/resources/certificates>.
2. In Keychain Access, open *My Certificates*, right-click
   **Developer ID Application: … (TEAMID)** and choose **Export…**. Save it as a
   `.p12` with a strong password.

## 2. App Store Connect API key (for notarytool)

1. Go to <https://appstoreconnect.apple.com/access/integrations/api>, open
   **Team Keys**, and generate a key with the **Developer** role.
2. Download the `AuthKey_XXXXXXXXXX.p8` file. Apple lets you download it only
   once.
3. Note the **Key ID** and the **Issuer ID** shown above the list.

## 3. Repository secrets

```bash
base64 -i DeveloperID.p12 | gh secret set MACOS_CERTIFICATE --repo bisand/hansolo
gh secret set MACOS_CERTIFICATE_PASSWORD --repo bisand/hansolo
base64 -i AuthKey_XXXXXXXXXX.p8 | gh secret set APPLE_API_KEY --repo bisand/hansolo
gh secret set APPLE_API_KEY_ID --repo bisand/hansolo
gh secret set APPLE_API_ISSUER --repo bisand/hansolo
```

`gh secret set` without piped input asks for the value, so the password and IDs
never land in your shell history.

| Secret | Contents |
| --- | --- |
| `MACOS_CERTIFICATE` | base64 of the exported `.p12` |
| `MACOS_CERTIFICATE_PASSWORD` | its export password |
| `APPLE_API_KEY` | base64 of `AuthKey_*.p8` |
| `APPLE_API_KEY_ID` | the key's Key ID |
| `APPLE_API_ISSUER` | the Issuer ID |

With only the two certificate secrets, builds are signed but not notarised.

## Checking a build

```bash
codesign --verify --strict --verbose=2 /Volumes/HanSolo/HanSolo.app
spctl --assess --type execute --verbose=2 /Volumes/HanSolo/HanSolo.app
xcrun stapler validate HanSolo-<version>.dmg
```

`spctl` should say `source=Notarized Developer ID`.

## Packaging locally

`package.sh` is the same script CI runs:

```bash
cargo build --release -p hansolo
SIGNING_IDENTITY="Developer ID Application: … (TEAMID)" \
APPLE_API_KEY_PATH=~/AuthKey_XXXXXXXXXX.p8 APPLE_API_KEY_ID=… APPLE_API_ISSUER=… \
  packaging/macos/package.sh 0.1.0 target/release/hansolo
```
