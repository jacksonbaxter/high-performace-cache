# High Performance Cache
This is my first attempt in creating a high performance database system while learning Rust.

## Features
- Caching layer non-persistent (e.g. Redis) and persistent layer (e.g. Postgres SQL). Pick one of these strategies or come up with your own: cache-aside, query caching, write-behind, write-through, or cache prefetching. (See https://redis.com/wp-content/uploads/2023/04/redis-enterprise-for-caching.pdf)
- Support 5000 read requests per second with subsecond average latency on each request.
- Support 5000 write requests per second with subsecond average latency on each request.
- High Concurrency

## Installation
Clone the repository
```bash
git clone https://github.com/jacksonbaxter/high-performace-cache.git
```
Enter the project repository
```bash
cd high-performace-cache
```
Build and run the project with Docker
```bash
# Build and start
docker-compose up --build
```
To rebuild the project and restart the database
```bash
# Clean up first
docker-compose down --volumes --remove-orphans

# Rebuild and start
docker-compose up --build
```
