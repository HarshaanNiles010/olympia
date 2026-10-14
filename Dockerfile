# syntax=docker/dockerfile:1
FROM rust:1-bookworm AS build
WORKDIR /app
COPY . .
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/app/target \
    cargo build --release --locked && cp target/release/olympia /olympia

FROM debian:bookworm-slim
# pandoc converts docx/odt/epub/pptx/xlsx/csv/rst/org/tex; poppler (pdftohtml) reads pdf layout;
# LibreOffice (no GUI) upgrades legacy doc/xls/ppt to docx/xlsx/pptx for pandoc.
# Debian's pandoc is 2.17, which lacks pptx/xlsx/tsv, so take the upstream release instead.
ARG TARGETARCH
ARG PANDOC_VERSION=3.9.0.2
ADD https://github.com/jgm/pandoc/releases/download/${PANDOC_VERSION}/pandoc-${PANDOC_VERSION}-1-${TARGETARCH}.deb /tmp/pandoc.deb
RUN apt-get update \
    && apt-get install -y --no-install-recommends /tmp/pandoc.deb poppler-utils ca-certificates \
       libreoffice-writer-nogui libreoffice-calc-nogui libreoffice-impress-nogui \
    && rm -rf /var/lib/apt/lists/* /tmp/pandoc.deb
COPY --from=build /olympia /usr/local/bin/olympia
USER nobody
ENV BIND_ADDR=0.0.0.0:3000
EXPOSE 3000
CMD ["olympia"]
