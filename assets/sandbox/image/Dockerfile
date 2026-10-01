# tod's test and runtime image: a small Ubuntu with what tod needs on a
# machine that has no display. Build from the repository root:
#
#   docker build -f assets/sandbox/image/Dockerfile -t tod-test:base .
#   docker build -f assets/sandbox/image/Dockerfile -t tod-test:gui --build-arg GUI=1 .
#
# `base` is exactly what `tod-sandbox bake ubuntu:24.04 --agents` makes
# (assets/sandbox/bootstrap.sh is the one place that says what that is): sh,
# git, curl, scp/sftp-server, Node.js 22, the Claude Code ACP adapter, and
# Claude Code. `gui` adds what GPUI needs to run with no monitor, and nothing
# else: the WebKitGTK and GTK libraries tod links for its embedded web views, Xvfb (a virtual X11 framebuffer), Mesa's software Vulkan driver
# (lavapipe: the CPU stands in for the GPU; every other driver is removed), and the X11/xkb client libraries
# and one font it loads at start. See doc/cloud-sandboxes/test-image.md.
FROM ubuntu:24.04

COPY bootstrap.sh /opt/tod/bootstrap.sh
# Two things keep it small. The adapter and Claude Code each ship the same
# 240 MB native `claude`: one file hard-linked when they are identical (in
# the same layer, so the copy is not stored twice). And Node's headers and
# docs are not needed to run it.
RUN sh /opt/tod/bootstrap.sh --agents \
 && g=/usr/local/lib/node_modules \
 && a=$g/@agentclientprotocol/claude-agent-acp/node_modules/@anthropic-ai/claude-agent-sdk-linux-x64/claude \
 && b=$g/@anthropic-ai/claude-code/node_modules/@anthropic-ai/claude-code-linux-x64/claude \
 && if cmp -s "$a" "$b"; then ln -f "$b" "$a"; fi \
 && rm -rf /opt/tod/node/include /opt/tod/node/share /opt/tod/node/CHANGELOG.md \
           /var/lib/apt/lists/* /root/.npm /tmp/*

ARG GUI=0
RUN if [ "$GUI" = 1 ]; then \
      export DEBIAN_FRONTEND=noninteractive \
      && apt-get update -qq \
      && apt-get install -y -qq --no-install-recommends \
           xvfb xauth x11-utils \
           mesa-vulkan-drivers libvulkan1 \
           libxkbcommon-x11-0 libxcb-xkb1 libx11-xcb1 libxcb-shape0 libxcb-xfixes0 \
           libfontconfig1 fonts-dejavu-core \
           libwebkit2gtk-4.1-0 libgtk-3-0 libxdo3 \
      && for f in /usr/lib/x86_64-linux-gnu/libvulkan_*.so /usr/share/vulkan/icd.d/*.json; do \
           case "$f" in *lvp*) ;; *) rm -f "$f" ;; esac; done \
      && rm -rf /var/lib/apt/lists/*; \
    fi

# A display for anything started without one: `xvfb-run` makes its own, and
# this is the size GPUI is tested at.
ENV XVFB_WHD=1280x800x24 \
    VK_ICD_FILENAMES=/usr/share/vulkan/icd.d/lvp_icd.x86_64.json
WORKDIR /root
CMD ["sleep", "infinity"]
