#!/bin/sh
# Shares Terraform providers across every copy of a module in this sandbox:
# its disk is RAM, and each `terraform init` otherwise downloads ~750 MB into
# its own copy and can fill the root (ENG-2504). Agents launched by tod get the
# same two variables in their environment (tod_sandbox::node::terraform_cache_env);
# this covers interactive shells.
# Idempotent. Run as root: by bootstrap.sh, and by tod on every install, fork
# and readying, so a sandbox made before this existed gets it too.
#
# Test hook: TOD_TF_ROOT prefixes every path (a temp directory).
set -eu

root="${TOD_TF_ROOT:-}"
cache=/root/.terraform.d/plugin-cache
mkdir -p "$root$cache"
mkdir -p "$root/etc/profile.d"
cat > "$root/etc/profile.d/tod-terraform.sh" <<EOF
export TF_PLUGIN_CACHE_DIR=$cache
export TF_PLUGIN_CACHE_MAY_BREAK_DEPENDENCY_LOCK_FILE=1
EOF
