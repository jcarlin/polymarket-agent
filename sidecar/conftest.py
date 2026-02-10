import sys
from pathlib import Path

# Add sidecar directory to path so imports work
sidecar_dir = Path(__file__).parent
if str(sidecar_dir) not in sys.path:
    sys.path.insert(0, str(sidecar_dir))
