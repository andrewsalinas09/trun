"""Dies right away with an import error, like a broken environment."""
print("starting training")
import torch_that_does_not_exist  # noqa: F401,E402
