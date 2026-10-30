# A real browser on a virtual display, for services that keep a logged-in session (see
# pi/skills/browser-login). A job builds on it with `FROM botler/image-browser`.
#   Chromium   DevTools (CDP) on 127.0.0.1:9223, forwarded to :9222 for the services network
#   Xvfb       display :99
#   x11vnc     localhost:5900 only; password re-read from $VNC_PASSWORD_FILE on every connect
#   noVNC      :6080 (websockify), the page a login visit opens; publish it only with approval
# The Chrome profile lives in /data/chrome-profile: the job's volume (`volume = true`).
# Chromium, Xvfb and x11vnc come from Debian bookworm and follow its security updates; noVNC and
# websockify, the parts reachable from outside, are pinned here.
FROM botler/agent-base:latest

ARG NOVNC_VERSION=1.5.0
ARG NOVNC_SHA256=6a73e41f98388a5348b7902f54b02d177cb73b7e5eb0a7a0dcf688cc2c79b42a
ARG WEBSOCKIFY_VERSION=0.12.0

RUN apt-get update \
 && apt-get install -y --no-install-recommends chromium xvfb x11vnc socat fonts-liberation \
 && rm -rf /var/lib/apt/lists/*

RUN curl -fsSL -o /tmp/novnc.tar.gz "https://github.com/novnc/noVNC/archive/refs/tags/v${NOVNC_VERSION}.tar.gz" \
 && echo "${NOVNC_SHA256}  /tmp/novnc.tar.gz" | sha256sum -c - \
 && mkdir /opt/novnc && tar -xzf /tmp/novnc.tar.gz -C /opt/novnc --strip-components=1 && rm /tmp/novnc.tar.gz \
 && uv venv -q /opt/websockify && uv pip install -q --python /opt/websockify/bin/python "websockify==${WEBSOCKIFY_VERSION}"

COPY images/browser/browser-session /usr/local/bin/browser-session
RUN chmod 755 /usr/local/bin/browser-session
ENV DISPLAY=:99 \
    VNC_PASSWORD_FILE=/run/vnc/password
