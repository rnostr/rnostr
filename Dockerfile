# This image uses cargo-chef to build the application in order to compile
# the dependencies apart from the main application. This allows the compiled
# dependencies to be cached in the Docker layer and greatly reduce the
# build time when there isn't any dependency changes.
#
# https://github.com/LukeMathWalker/cargo-chef
# https://github.com/RGB-WG/rgb-node/blob/master/Dockerfile

ARG SRC_DIR=/usr/local/src/rnostr
ARG BUILDER_DIR=/srv/rnostr

# build base
ARG BASE=base

# Base image
FROM rust:1.71.1-slim-bullseye as base

# mirror image for china
FROM base as mirror_cn

# Replace cn mirrors
ENV RUSTUP_DIST_SERVER=https://rsproxy.cn
RUN sed -i 's/deb.debian.org/mirrors.163.com/g' /etc/apt/sources.list
RUN echo '[source.crates-io]\nreplace-with = "mirror"\n[source.mirror]\nregistry = "https://rsproxy.cn/crates.io-index"' \
        >> $CARGO_HOME/config

FROM ${BASE} as chef

ARG SRC_DIR
ARG BUILDER_DIR

RUN apt-get update && apt-get install -y build-essential

RUN rustup default stable
RUN rustup update
RUN cargo install cargo-chef --locked

WORKDIR $SRC_DIR

# Cargo chef step that analyzes the project to determine the minimum subset of
# files (Cargo.lock and Cargo.toml manifests) required to build it and cache
# dependencies
FROM chef AS planner

COPY . .
RUN cargo chef prepare --recipe-path recipe.json

FROM chef AS builder

ARG SRC_DIR
ARG BUILDER_DIR

COPY --from=planner "${SRC_DIR}/recipe.json" recipe.json

# Build dependencies - this is the caching Docker layer
RUN cargo chef cook --release --recipe-path recipe.json --target-dir "${BUILDER_DIR}"

# Copy all files and build application # --all-features
COPY . .
RUN cargo build --release --target-dir "${BUILDER_DIR}" --bins

# Final image with binaries
FROM debian:bullseye-slim as final

ARG SRC_DIR
ARG BUILDER_DIR

ARG BIN_DIR=/usr/local/bin
ARG HOME_DIR=/rnostr
ARG DATA_DIR=${HOME_DIR}/data
# use dir for watch config file change
ARG CONF_DIR=${HOME_DIR}/config
ARG USER=rnostr

RUN adduser --home "${HOME_DIR}" --shell /bin/bash --disabled-login \
        --gecos "${USER} user" ${USER}
RUN mkdir ${DATA_DIR} ${CONF_DIR}
RUN chown ${USER}:${USER} ${DATA_DIR} ${CONF_DIR}

COPY --from=builder --chown=${USER}:${USER} \
        "${BUILDER_DIR}/release/rnostr" "${BIN_DIR}"
COPY --from=builder --chown=${USER}:${USER} \
        "${SRC_DIR}/rnostr.example.toml" "${CONF_DIR}/rnostr.toml"

# Change default bind address to 0.0.0.0
ENV RNOSTR_NETWORK__HOST=0.0.0.0

WORKDIR "${HOME_DIR}"

USER ${USER}

VOLUME "$DATA_DIR"
VOLUME "$CONF_DIR"

EXPOSE 8080

ENTRYPOINT ["rnostr"]

CMD ["relay", "--watch", "-c", "./config/rnostr.toml"]
