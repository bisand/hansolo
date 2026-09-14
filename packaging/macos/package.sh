#!/usr/bin/env bash
# Packages a built macOS binary as dist/hansolo-<version>-universal-macos.tar.gz
# and dist/HanSolo-<version>.dmg.
#
#   packaging/macos/package.sh <version> <binary>
#
# With SIGNING_IDENTITY set (a "Developer ID Application: …" certificate in the
# keychain) the binary, app and disk image are signed with the hardened runtime.
# With APPLE_API_KEY_PATH, APPLE_API_KEY_ID and APPLE_API_ISSUER also set, they
# are notarised and the tickets stapled, so Gatekeeper opens them without asking.
# Without either, everything is ad-hoc signed only.
set -euo pipefail

version=$1
binary=$2
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
mkdir -p dist

sign() {
    if [[ -n ${SIGNING_IDENTITY:-} ]]; then
        codesign --force --timestamp --options runtime --sign "$SIGNING_IDENTITY" "$@"
    else
        codesign --force --sign - "$@"
    fi
}

notarize() {
    xcrun notarytool submit "$1" --wait \
        --key "$APPLE_API_KEY_PATH" --key-id "$APPLE_API_KEY_ID" --issuer "$APPLE_API_ISSUER"
}

can_notarize() {
    [[ -n ${SIGNING_IDENTITY:-} && -n ${APPLE_API_KEY_PATH:-} && -n ${APPLE_API_KEY_ID:-} && -n ${APPLE_API_ISSUER:-} ]]
}

# The bare binary for the tarball, and HanSolo.app around its own copy (signing
# a bundle changes the executable's signature, so they are notarised together).
name="hansolo-$version-universal-macos"
mkdir "$work/$name"
cp "$binary" "$work/$name/hansolo"
sign "$work/$name/hansolo"

app="$work/dmg/HanSolo.app"
mkdir -p "$app/Contents/MacOS"
cp "$binary" "$app/Contents/MacOS/hansolo"
sed "s/@VERSION@/$version/g" "$(dirname "$0")/Info.plist" > "$app/Contents/Info.plist"
sign "$app"

if can_notarize; then
    # One submission carries both.
    mkdir "$work/submit"
    cp -R "$work/$name/hansolo" "$app" "$work/submit/"
    ditto -c -k "$work/submit" "$work/submit.zip"
    notarize "$work/submit.zip"
    # A bare Mach-O cannot hold a ticket; Gatekeeper finds it online instead.
    xcrun stapler staple "$app"
fi

cp "$(dirname "$0")/../../README.md" "$work/$name/"
tar czf "dist/$name.tar.gz" -C "$work" "$name"

ln -s /Applications "$work/dmg/Applications"
dmg="dist/HanSolo-$version.dmg"
hdiutil create -volname HanSolo -srcfolder "$work/dmg" -format UDZO -ov "$dmg"
sign "$dmg"
if can_notarize; then
    notarize "$dmg"
    xcrun stapler staple "$dmg"
fi

codesign --verify --strict --verbose=2 "$app"
if can_notarize; then
    spctl --assess --type open --context context:primary-signature --verbose=2 "$dmg"
fi
ls -l dist
