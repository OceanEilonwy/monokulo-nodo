# Everything every build needs, baked once so runs do not update or download it.
ARG VOID_IMAGE
FROM ${VOID_IMAGE}
ARG VOID_MIRROR
ARG VOID_PACKAGES_REV
RUN mkdir -p /etc/xbps.d \
 && echo "repository=${VOID_MIRROR}/current" > /etc/xbps.d/00-repository-main.conf \
 && xbps-install -Syu xbps \
 && xbps-install -yu \
 && xbps-install -y bash git curl file openssh qemu-system-aarch64 qemu-img e2fsprogs zstd cpio tar
RUN git clone -q --filter=blob:none --no-checkout https://github.com/void-linux/void-packages.git /work/void-packages \
 && git -C /work/void-packages checkout -q "${VOID_PACKAGES_REV}" \
 && cd /work/void-packages \
 && printf 'XBPS_CHROOT_CMD=ethereal\nXBPS_ALLOW_CHROOT_BREAKOUT=yes\nXBPS_MIRROR=%s/current\n' "${VOID_MIRROR}" > etc/conf \
 && ln -s / masterdir \
 && ./xbps-src binary-bootstrap
# Kept as its own layer so adding it did not repeat the steps above.
RUN xbps-install -Sy qemu-system-amd64
