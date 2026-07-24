import time

start = time.perf_counter()
acc = 0
for i in range(300000):
    acc += 1
end = time.perf_counter()
print(f"elapsed = {end - start} seconds")
