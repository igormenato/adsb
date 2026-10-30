package main

import (
	"context"
	"sync"
)

const queueCap = 64

type Queue struct {
	mu     sync.Mutex
	cond   *sync.Cond
	frames []Frame
	closed bool
}

func NewQueue() *Queue {
	q := &Queue{frames: make([]Frame, 0, queueCap)}
	q.cond = sync.NewCond(&q.mu)
	return q
}

func (q *Queue) Close() {
	q.mu.Lock()
	q.closed = true
	q.cond.Broadcast()
	q.mu.Unlock()
}

func (q *Queue) Push(f Frame) (dropped Frame, didDrop bool) {
	q.mu.Lock()
	defer q.mu.Unlock()
	if q.closed {
		return f, true
	}
	if len(q.frames) < queueCap {
		q.frames = append(q.frames, f)
		q.cond.Signal()
		return Frame{}, false
	}
	for i, old := range q.frames {
		if old[27] == 0 {
			dropped = old
			q.frames = append(q.frames[:i], q.frames[i+1:]...)
			q.frames = append(q.frames, f)
			q.cond.Signal()
			return dropped, true
		}
	}
	if f[27] == 0 {
		return f, true
	}
	dropped = q.frames[0]
	q.frames = append(q.frames[1:], f)
	q.cond.Signal()
	return dropped, true
}

func (q *Queue) PopWait(ctx context.Context) (Frame, bool) {
	q.mu.Lock()
	defer q.mu.Unlock()
	stop := context.AfterFunc(ctx, func() {
		q.mu.Lock()
		q.cond.Broadcast()
		q.mu.Unlock()
	})
	defer stop()
	for len(q.frames) == 0 && !q.closed && ctx.Err() == nil {
		q.cond.Wait()
	}
	if len(q.frames) == 0 {
		return Frame{}, false
	}
	f := q.frames[0]
	q.frames = q.frames[1:]
	return f, true
}

func (q *Queue) snapshot() []Frame {
	q.mu.Lock()
	defer q.mu.Unlock()
	out := make([]Frame, len(q.frames))
	copy(out, q.frames)
	return out
}
