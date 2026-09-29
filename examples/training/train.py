"""Demo training script for trun: steps, progress, metrics, and a NaN spike.

    trun run -- python examples/training/train.py

Uses the optional helper (sdk/python/trun.py); without trun it runs silently fine.
"""
import math
import os
import random
import sys
import time

sys.path.insert(0, os.path.join(os.path.dirname(__file__), "..", "..", "sdk", "python"))
import trun  # noqa: E402

EPOCHS = int(os.environ.get("EPOCHS", "6"))
STEPS_PER_EPOCH = 50

with trun.step("setup", "Load dataset"):
    print("loading 12,000 samples")
    time.sleep(0.5)

step = 0
loss = 2.5
with trun.step("train", "Train"):
    for epoch in range(1, EPOCHS + 1):
        trun.step_begin(f"epoch-{epoch}", f"Epoch {epoch}", parent="train")
        for b in range(1, STEPS_PER_EPOCH + 1):
            step += 1
            loss = max(0.05, loss * (0.985 + random.random() * 0.01))
            grad = math.nan if (epoch == 4 and b == 25) else loss * (1.5 + random.random())
            trun.metric(step=step, loss=loss, grad_norm=grad, samples_per_s=900 + random.random() * 250)
            trun.progress(f"epoch-{epoch}", b, STEPS_PER_EPOCH, unit="batch")
            time.sleep(0.04)
        val = loss * 1.08
        trun.metric(step=step, val_loss=val)
        trun.progress("train", epoch, EPOCHS, unit="epoch")
        trun.step_end(f"epoch-{epoch}")
        print(f"epoch {epoch}: loss={loss:.4f} val_loss={val:.4f}")
        if epoch == 4:
            trun.warn("grad_norm was NaN once in epoch 4 (skipped the batch)")
trun.note(f"finished {EPOCHS} epochs, final loss {loss:.4f}")
print("done")
