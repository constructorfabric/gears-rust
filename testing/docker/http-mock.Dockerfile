FROM python:3.14-slim@sha256:a2b82f3c48559aa0a8446d9af49826b6e2b2016f4cd2afabfe6013ec53729170

WORKDIR /app

# Copy testdata and server files.
# Paths are relative to the build context, which docker-compose.yml sets to
# `testing/` (context: ..). A leading `../` escapes the context and Docker
# rejects the COPY outright.
COPY e2e/testdata /app/testdata
COPY e2e/helpers/mock_server.py /app/mock_server.py
COPY e2e/helpers/run_mock_server.py /app/run_mock_server.py

# Expose port 8080
EXPOSE 8080

# Run the mock server
RUN useradd -U -u 1000 appuser && \
    chown -R 1000:1000 /app
USER 1000
CMD ["python", "-u", "/app/run_mock_server.py"]
