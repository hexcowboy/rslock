all: start test stop

start:
	redis-server --save "" --port 6380 &
	redis-server --save "" --port 6381 &
	redis-server --save "" --port 6382 &

stop:
	redis-cli -p 6380 shutdown nosave
	redis-cli -p 6381 shutdown nosave
	redis-cli -p 6382 shutdown nosave

test:
	cargo test
