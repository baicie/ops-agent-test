FROM python:3.12.11-alpine3.22

RUN addgroup -S app && adduser -S -G app app

WORKDIR /app
COPY --chown=app:app app.py /app/app.py

ENV PYTHONUNBUFFERED=1
USER app

EXPOSE 8080
HEALTHCHECK --interval=5s --timeout=2s --start-period=2s --retries=5 \
  CMD wget -q -O - http://127.0.0.1:8080/health >/dev/null || exit 1

CMD ["python", "/app/app.py"]
