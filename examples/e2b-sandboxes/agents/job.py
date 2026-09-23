"""The job the runner is allowed to run, once a human says yes."""
import hashlib
import platform
import time

t0 = time.perf_counter()
digest = hashlib.sha256()
for i in range(200_000):
    digest.update(str(i).encode())
print(f"nightly job ok on {platform.node()}: sha256 {digest.hexdigest()[:16]} "
      f"in {(time.perf_counter() - t0) * 1000:.0f} ms")
