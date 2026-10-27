#!/usr/bin/env python3
"""A minimal ferryd client: one connection, requests with their answers, and events."""
import json
import os
import socket


class ApiError(Exception):
	"""An error answer. code is the machine word, str() is the message, hint may be None."""

	def __init__(self, error):
		super().__init__(error["message"])
		self.code = error["code"]
		self.hint = error.get("hint")


class Ferry:
	def __init__(self, path=None):
		runtime = os.environ.get("XDG_RUNTIME_DIR", f"/run/user/{os.getuid()}")
		self.sock = socket.socket(socket.AF_UNIX)
		self.sock.connect(path or os.path.join(runtime, "ferry", "ferryd.sock"))
		self.lines = self.sock.makefile("r", encoding="utf-8", newline="\n")
		self.next_id = 1
		self.pending = []  # events that arrived while a call waited for its answer
		first = self.read()
		assert first["event"] == "hello", first
		self.hello = first["data"]

	def read(self):
		line = self.lines.readline()
		if not line:
			raise EOFError("ferryd closed the connection")
		return json.loads(line)

	def supports(self, method):
		# an M1 ferryd sends no list of methods
		return method in self.hello.get("methods", [])

	def call(self, method, params=None):
		request = {"id": self.next_id, "method": method}
		if params is not None:
			request["params"] = params
		self.next_id += 1
		self.sock.sendall((json.dumps(request) + "\n").encode())
		while True:
			frame = self.read()
			if "event" in frame:
				self.pending.append(frame)
			elif frame["id"] == request["id"]:
				if "error" in frame:
					raise ApiError(frame["error"])
				return frame["result"]

	def event(self):
		"""The next event, from the ones that arrived during a call or from the socket."""
		while not self.pending:
			frame = self.read()
			if "event" in frame:
				self.pending.append(frame)
		return self.pending.pop(0)


if __name__ == "__main__":
	ferry = Ferry()
	print("hello:", ferry.hello["peer"], "api", ferry.hello["api"])
	if ferry.supports("subscribe"):
		ferry.call("subscribe", {"topics": ["state"]})
		# the snapshot that follows the answer; after it, one event for each change
		event = ferry.event()
		print(event["event"], event["data"]["summary"])
	else:
		print("status:", ferry.call("status")["role"])
	try:
		ferry.call("config.set", {"key": "crossing.edge_resistance_px", "value": 900})
	except ApiError as error:
		print(error.code, "-", error, "-", error.hint)
