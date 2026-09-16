FROM python:3.14-slim@sha256:cad9a2c871761c413caa6fdd6441c783451e740a48aaeba60ae62a8b53525ef6

WORKDIR /app

# Copy testdata and server files
COPY ../e2e/testdata /app/testdata
COPY ../e2e/helpers/mock_server.py /app/mock_server.py
COPY ../e2e/helpers/run_mock_server.py /app/run_mock_server.py

# Expose port 8080
EXPOSE 8080

# Run the mock server
RUN useradd -U -u 1000 appuser && \
    chown -R 1000:1000 /app
USER 1000
CMD ["python", "-u", "/app/run_mock_server.py"]
