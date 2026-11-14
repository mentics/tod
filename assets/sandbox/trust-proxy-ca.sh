#!/bin/sh
# Makes the sandbox's intercepting HTTPS proxy CA trusted system-wide, so tools
# that use the system trust store and ignore SSL_CERT_FILE (apt's gnutls, pip,
# Java, git built with gnutls) verify the certificates the proxy presents.
#
# Does nothing when no proxy is configured (HTTPS_PROXY / https_proxy unset).
# With a proxy set, a missing CA file or a failed install is an error.
# Idempotent. Run as root, on every install and every fork (a fork's proxy
# update replaces the files).
#
# Test hooks: TOD_CA_ROOT prefixes every path (a temp directory),
# TOD_CA_SKIP_UPDATE=1 skips running the distribution's update command.
set -eu

proxy="${HTTPS_PROXY:-${https_proxy:-}}"
if [ -z "$proxy" ]; then
    echo "trust-proxy-ca: no HTTPS proxy configured; nothing to do"
    exit 0
fi

root="${TOD_CA_ROOT:-}"
ca="$root/etc/ssl/certs/proxy/proxy-ca.pem"
if [ ! -s "$ca" ]; then
    echo "trust-proxy-ca: HTTPS_PROXY is set ($proxy) but the proxy CA $ca is missing or empty" >&2
    exit 1
fi

have() { command -v "$1" >/dev/null 2>&1; }

if have update-ca-certificates; then
    dest="$root/usr/local/share/ca-certificates/tod-proxy-ca.crt"
    update="update-ca-certificates"
    bundle=/etc/ssl/certs/ca-certificates.crt
elif have update-ca-trust; then
    dest="$root/etc/pki/ca-trust/source/anchors/tod-proxy-ca.crt"
    update="update-ca-trust extract"
    bundle=/etc/pki/tls/certs/ca-bundle.crt
elif [ -n "${TOD_CA_SKIP_UPDATE:-}" ]; then
    dest="$root/usr/local/share/ca-certificates/tod-proxy-ca.crt"
    update=""
    bundle=/etc/ssl/certs/ca-certificates.crt
else
    echo "trust-proxy-ca: neither update-ca-certificates nor update-ca-trust is installed (the ca-certificates package is missing); cannot trust $ca" >&2
    exit 1
fi

if [ -f "$dest" ] && cmp -s "$ca" "$dest"; then
    echo "trust-proxy-ca: already trusted"
else
    mkdir -p "$(dirname "$dest")"
    cp "$ca" "$dest"
    chmod 0644 "$dest"
    if [ -n "${TOD_CA_SKIP_UPDATE:-}" ]; then
        echo "trust-proxy-ca: wrote $dest (update skipped)"
    else
        echo "trust-proxy-ca: running $update"
        $update
    fi
fi

# Python (requests, pip) uses certifi's own bundle unless told otherwise:
# point it at the system bundle, which now holds the proxy CA.
mkdir -p "$root/etc/profile.d"
cat > "$root/etc/profile.d/tod-ca.sh.tmp" <<PROFILE
export REQUESTS_CA_BUNDLE=$bundle
export PIP_CERT=$bundle
export CURL_CA_BUNDLE=$bundle
PROFILE
mv -f "$root/etc/profile.d/tod-ca.sh.tmp" "$root/etc/profile.d/tod-ca.sh"
if [ ! -f "$root/etc/pip.conf" ]; then
    printf '[global]\ncert = %s\n' "$bundle" > "$root/etc/pip.conf"
elif ! grep -q '^[[:space:]]*cert[[:space:]]*=' "$root/etc/pip.conf"; then
    echo "trust-proxy-ca: $root/etc/pip.conf exists without a cert; leaving it (PIP_CERT is exported by /etc/profile.d/tod-ca.sh)"
fi
echo "trust-proxy-ca: ready"
