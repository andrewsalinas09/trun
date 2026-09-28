"""Fake training loop: epochs with a \\r batch bar, colored log lines, ~40s total."""
import random
import sys
import time

EPOCHS = int(sys.argv[1]) if len(sys.argv) > 1 else 8
BATCHES = 20

print("\x1b[1mtrainer\x1b[0m: 2 GPUs, batch size 64")
loss = 2.5
for epoch in range(1, EPOCHS + 1):
    for b in range(1, BATCHES + 1):
        loss *= 0.985 + random.random() * 0.01
        bar = "#" * (b * 20 // BATCHES)
        sys.stdout.write(f"\repoch {epoch}/{EPOCHS} [{bar:<20}] {b}/{BATCHES} loss={loss:.4f}")
        sys.stdout.flush()
        time.sleep(0.25)
    sys.stdout.write("\n")
    print(f"\x1b[32mepoch {epoch} done\x1b[0m: val_loss={loss * 1.07:.4f}")
    if epoch == 3:
        print("\x1b[33mwarning\x1b[0m: learning rate plateau, reducing to 1e-4", file=sys.stderr)
print("training complete")
