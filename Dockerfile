FROM clux/muslrust:stable AS chef
USER root
RUN cargo install cargo-chef
WORKDIR /app

FROM node:21 AS frontend
WORKDIR /app
COPY ui .
RUN npm i && npm run build

FROM chef AS planner
COPY backend .
RUN cargo chef prepare --recipe-path recipe.json

FROM chef AS builder
ARG TARGETARCH
COPY --from=planner /app/recipe.json recipe.json
COPY platform.sh .
RUN chmod +x platform.sh
RUN ./platform.sh
RUN curl -sSL $(curl -s https://api.github.com/repos/upx/upx/releases/latest \
    | grep browser_download_url | grep $TARGETARCH | cut -d '"' -f 4) -o upx.tar.xz
RUN tar -xf upx.tar.xz \
    && cd upx-*-${TARGETARCH}_linux \
    && mv upx /bin/upx
RUN cargo chef cook --release --target $(cat /.platform) --recipe-path recipe.json
COPY backend .
RUN cargo build --release --target $(cat /.platform) --bin links
RUN mv ./target/$(cat /.platform)/release/links ./links
RUN upx --best --lzma ./links

FROM scratch AS runtime
COPY --from=builder /app/links /app
COPY --from=builder /app/db /db
COPY --from=frontend /app/dist /static
EXPOSE 3000
CMD ["/app"]