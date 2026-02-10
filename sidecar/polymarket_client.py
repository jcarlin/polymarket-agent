"""Polymarket CLOB client wrapper for the Python sidecar.

Handles order signing and placement via py-clob-client.
Phase 1: scaffold only — trading methods added in Phase 3.
"""

import logging
import os
from typing import Optional

logger = logging.getLogger("sidecar.polymarket_client")

CLOB_HOST = "https://clob.polymarket.com"
CHAIN_ID = 137  # Polygon mainnet


class PolymarketClient:
    """Wrapper around py-clob-client for order signing and placement."""

    def __init__(self) -> None:
        self._client = None
        self._initialized = False

    def initialize(self) -> bool:
        """Initialize the CLOB client. Returns True if successful.

        Fails gracefully if private key is not set or py-clob-client
        is not available.
        """
        private_key = os.getenv("POLYMARKET_WALLET_PRIVATE_KEY")
        if not private_key:
            logger.warning("POLYMARKET_WALLET_PRIVATE_KEY not set — client not initialized")
            return False

        try:
            from py_clob_client.client import ClobClient

            self._client = ClobClient(
                CLOB_HOST,
                key=private_key,
                chain_id=CHAIN_ID,
                signature_type=0,  # EOA wallet
            )
            # Derive API credentials
            self._client.set_api_creds(self._client.create_or_derive_api_creds())
            self._initialized = True
            logger.info("Polymarket CLOB client initialized successfully")
            return True
        except ImportError:
            logger.warning("py-clob-client not installed — client not initialized")
            return False
        except Exception as e:
            logger.error("Failed to initialize CLOB client: %s", e)
            return False

    @property
    def is_initialized(self) -> bool:
        return self._initialized

    @property
    def client(self) -> Optional[object]:
        """Access the underlying ClobClient. None if not initialized."""
        return self._client
