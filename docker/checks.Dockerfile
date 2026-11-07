# Small image for checks that finish in seconds and never boot a VM.
ARG VOID_IMAGE
FROM ${VOID_IMAGE}
ARG VOID_MIRROR
RUN mkdir -p /etc/xbps.d \
 && echo "repository=${VOID_MIRROR}/current" > /etc/xbps.d/00-repository-main.conf \
 && xbps-install -Syu xbps \
 && xbps-install -yu \
 && xbps-install -y shellcheck nftables python3 bash
