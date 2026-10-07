FROM python:3.14-slim@sha256:51dafde81dbdb6ebde285137a295cf18a47ca95234fe388a343719cb97305b3d

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
