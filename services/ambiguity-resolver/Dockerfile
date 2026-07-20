FROM python:3.11-slim

ENV PYTHONDONTWRITEBYTECODE=1 \
    PYTHONUNBUFFERED=1

WORKDIR /app

COPY services/ambiguity-resolver/pyproject.toml ./
COPY services/ambiguity-resolver/app ./app

RUN pip install --no-cache-dir . \
    && addgroup --system resolver \
    && adduser --system --ingroup resolver resolver

USER resolver

EXPOSE 8000

CMD ["uvicorn", "app.main:app", "--host", "0.0.0.0", "--port", "8000"]
