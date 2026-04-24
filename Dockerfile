FROM python:3.14-slim AS builder

WORKDIR /app
COPY --from=ghcr.io/astral-sh/uv:latest /uv /usr/local/bin/uv

ENV UV_COMPILE_BYTECODE=1 \
    UV_LINK_MODE=copy \
    UV_PYTHON_DOWNLOADS=never

COPY pyproject.toml uv.lock ./
COPY src/ src/

# hatch-vcs needs a git tag to determine the version.
# Without .git in the build context, pass it as a build arg instead.
ARG VERSION=0.0.0.dev0
RUN SETUPTOOLS_SCM_PRETEND_VERSION_FOR_NX_BOSS=${VERSION} uv sync --no-dev --no-editable


FROM python:3.14-slim

WORKDIR /app

COPY --from=builder /app/.venv /app/.venv

ENV PATH="/app/.venv/bin:$PATH"

RUN adduser --disabled-password --gecos "" appuser \
    && mkdir -p /data \
    && chown appuser:appuser /data
USER appuser

VOLUME ["/data", "/config"]
EXPOSE 10447

ENTRYPOINT ["python", "-m", "nx_boss"]
CMD ["--config", "/config/config.yaml", "--host", "0.0.0.0", "--port", "10447"]
