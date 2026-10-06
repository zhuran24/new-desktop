#!/usr/bin/env python3
"""真实测试 MCP 工具：等待 FIFO 后返回结果，同时记录服务端开始、取消通知与完成。"""
import json
from pathlib import Path
import sys
import threading

lock=threading.Lock()
root=Path('/sandbox/project')

def handle(message):
    method=message.get('method')
    if method=='notifications/cancelled':
        (root/'mcp-cancelled').write_text(json.dumps(message))
    if 'id' not in message: return
    if method=='initialize':
        result={'protocolVersion':message['params']['protocolVersion'],'capabilities':{'tools':{}},'serverInfo':{'name':'fifo','version':'1'}}
    elif method=='tools/list':
        result={'tools':[{'name':'wait','description':'Wait for the local FIFO','inputSchema':{'type':'object','properties':{}}}]}
    elif method=='tools/call':
        (root/'mcp-started').touch()
        with open('/sandbox/fifos/mcp') as fifo: fifo.readline()
        (root/'mcp-finished').touch()
        result={'content':[{'type':'text','text':'MCP_FINISHED'}]}
    else: result={}
    with lock:
        print(json.dumps({'jsonrpc':'2.0','id':message['id'],'result':result}),flush=True)

for line in sys.stdin:
    threading.Thread(target=handle,args=(json.loads(line),),daemon=True).start()
