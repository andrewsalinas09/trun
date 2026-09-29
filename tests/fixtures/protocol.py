"""Reports structure with :: lines on stdout (no library)."""
import time

print("::step-begin id=setup name=\"Load data\"")
print("loading 3 shards")
time.sleep(0.3)
print("::step-end setup ok")

print("::step-begin id=train name=\"Train\"")
for epoch in range(1, 6):
    print(f"::step-begin id=epoch-{epoch} parent=train")
    for b in range(1, 11):
        print(f"::progress epoch-{epoch} {b}/10")
        time.sleep(0.03)
    loss = 1.0 / epoch
    grad = float("nan") if epoch == 4 else loss * 2
    print(f"::metric loss={loss:.4f} val_loss={loss * 1.1:.4f} grad_norm={grad} step={epoch}")
    print(f"::progress train {epoch}/5 unit=epoch")
    print(f"epoch {epoch}: loss {loss:.4f}")
print("::note switched to lr schedule B after epoch 3")
print("::warn validation set smaller than expected")
print("::step-end train")
print("::metric loss=oops")
print("finished")
