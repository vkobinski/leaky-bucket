import asyncio
import aiohttp
import time
from collections import defaultdict

async def make_request(session, url, results):
    start_time = time.monotonic()
    try:
        async with session.get(url) as response:
            latency = time.monotonic() - start_time
            results[url].append((latency, response.status))
    except Exception as e:
        latency = time.monotonic() - start_time
        results[url].append((latency, str(e)))

async def test_endpoint(url, num_requests, concurrency):
    results = defaultdict(list)
    connector = aiohttp.TCPConnector(limit=concurrency)
    async with aiohttp.ClientSession(connector=connector) as session:
        tasks = [make_request(session, url, results) for _ in range(num_requests)]
        await asyncio.gather(*tasks)
    return results[url]

def analyze_results(results, burst_capacity, expected_delay, tolerance=0.2):
    # Sort results by latency to identify burst vs delayed requests
    sorted_results = sorted(results, key=lambda x: x[0])
    
    # Split into burst and delayed groups
    burst_group = sorted_results[:burst_capacity]
    delayed_group = sorted_results[burst_capacity:]
    
    # Calculate average latencies
    avg_burst_latency = sum(l for l, _ in burst_group) / len(burst_group) if burst_group else 0
    avg_delayed_latency = sum(l for l, _ in delayed_group) / len(delayed_group) if delayed_group else 0
    
    print(f"\n{'='*50}")
    print(f"Analysis for {len(results)} requests:")
    print(f"Burst capacity: {burst_capacity}")
    print(f"Fast requests (expected {burst_capacity}): {len(burst_group)}")
    print(f"Delayed requests: {len(delayed_group)}")
    print(f"Average burst latency: {avg_burst_latency:.3f}s")
    print(f"Average delayed latency: {avg_delayed_latency:.3f}s (expected ~{expected_delay}s)")

    # Validate results
    passed = True
    
    # Check burst group latency
    if any(latency > 0.5 for latency, _ in burst_group):
        print("FAIL: Some burst requests had high latency")
        passed = False
        
    # Check delayed group timing
    expected_min_delay = expected_delay * (1 - tolerance)
    if delayed_group and avg_delayed_latency < expected_min_delay:
        print(f"FAIL: Delayed requests too fast (expected min {expected_min_delay:.1f}s)")
        passed = False
        
    # Check request success rate
    success_count = sum(1 for _, status in results if status == 200)
    if success_count != len(results):
        print(f"FAIL: Had {len(results) - success_count} failed requests")
        passed = False
        
    print("TEST PASSED" if passed else "TEST FAILED")
    print('='*50)

async def main():
    base_url = "http://localhost:3000"
    
    # Test configuration
    test_cases = [
        {
            "path": "/",
            "burst_capacity": 1000,  # PJ category burst size
            "requests": 1010,        # 1000 burst + 100 delayed
            "expected_delay": 3.0,   # 1 / (20/60) = 3s per request
            "concurrency": 100
        },
        {
            "path": "/claims",
            "burst_capacity": 18000,  # ClaimsRead burst size
            "requests": 2000,        # Well below burst capacity
            "expected_delay": 0.0,    # No delays expected
            "concurrency": 100
        }
    ]

    for case in test_cases:
        url = base_url + case["path"]
        print(f"\nStarting test for {url} with {case['requests']} requests...")
        start_time = time.monotonic()
        
        results = await test_endpoint(
            url,
            case["requests"],
            case["concurrency"]
        )
        
        total_time = time.monotonic() - start_time
        print(f"Completed {case['requests']} requests in {total_time:.1f} seconds")
        
        analyze_results(
            results,
            case["burst_capacity"],
            case["expected_delay"]
        )

if __name__ == "__main__":
    asyncio.run(main())