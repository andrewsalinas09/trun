"""An unmodified tqdm training loop: trun must show progress + ETA with no changes."""
import math
import time

from tqdm import tqdm

EPOCHS = 3
BATCHES = 40

loss = 2.0
for epoch in range(1, EPOCHS + 1):
    bar = tqdm(range(BATCHES), desc=f"Epoch {epoch}")
    for _ in bar:
        loss *= 0.99
        bar.set_postfix(loss=f"{loss:.4f}")
        time.sleep(0.05)
    print(f"epoch {epoch} val_loss={loss * 1.05:.4f}")
print("done", math.isfinite(loss))
