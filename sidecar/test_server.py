import os

import pytest
import httpx
from httpx import ASGITransport

from server import app


@pytest.fixture
def client():
    transport = ASGITransport(app=app)
    return httpx.AsyncClient(transport=transport, base_url="http://test")


@pytest.mark.asyncio
async def test_health_returns_200(client):
    response = await client.get("/health")
    assert response.status_code == 200


@pytest.mark.asyncio
async def test_health_response_schema(client):
    response = await client.get("/health")
    data = response.json()
    assert data["status"] == "ok"
    assert data["version"] == "0.1.0"
    assert "trading_mode" in data


@pytest.mark.asyncio
async def test_health_reflects_trading_mode(client, monkeypatch):
    # The trading mode is read at module import time, so we test the default
    response = await client.get("/health")
    data = response.json()
    assert data["trading_mode"] == os.getenv("TRADING_MODE", "paper")


@pytest.mark.asyncio
async def test_unknown_endpoint_returns_404(client):
    response = await client.get("/nonexistent")
    assert response.status_code == 404


@pytest.mark.asyncio
async def test_order_returns_503_when_not_initialized(client):
    """In paper mode (default), client is not initialized so /order returns 503."""
    response = await client.post(
        "/order",
        json={"token_id": "tok_yes_1", "price": 0.55, "size": 5.0, "side": "BUY"},
    )
    assert response.status_code == 503


@pytest.mark.asyncio
async def test_order_validates_request_body(client):
    """Missing required fields should return 422."""
    response = await client.post("/order", json={"token_id": "tok_yes_1"})
    assert response.status_code == 422


@pytest.mark.asyncio
async def test_order_endpoint_exists(client):
    """Verify the /order endpoint is registered (not 404)."""
    response = await client.post(
        "/order",
        json={"token_id": "tok_1", "price": 0.5, "size": 1.0, "side": "BUY"},
    )
    # Should be 503 (not initialized) not 404 (not found)
    assert response.status_code != 404
