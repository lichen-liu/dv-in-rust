import sys
import subprocess
import os
import time
import argparse
import os
import signal

DEBUG = 1

if __name__ == "__main__":
    # use ps aux | grep client.py to check how many clients are running
    parser = argparse.ArgumentParser()

    parser.add_argument("--range", nargs='+', required=True)
    args = parser.parse_args()
    cid_range = args.range
    if (len(cid_range) != 2):
        print("This script takes exactly two argument! e.g. python launcher.py --range 0 10")
        sys.exit()

    cids = list(range(int(cid_range[0]), int(cid_range[1])))
    
    script = "client.py"
    if DEBUG:
        script = "test.py"

    procs = {}
    port = 4000

    print("Popen child processes...")

    # With subprocess.Popen(), all child processes terminates if the parent is terminated with ctrl-C
    # However, if parent has a bug/ throws exception, child processes continue running
    # -> handle this with finally
    for cid in cids:
        if DEBUG:
            command = "python3.8 {} --c_id {}".format(script, cid)
            procs[subprocess.Popen(["python3.8", script, "--c_id", str(cid)])] = command
            #procs.add(subprocess.Popen(["python3.8", script]))
        else:
            procs.add(subprocess.Popen("python3.8 {} --port {} --c_id {}".format(script, port, cid))) #, stderr=subprocess.PIPE))
            port = port + 1

    try:
        # p.poll() only gets return code
        # p.communicate() can get stdout and stderr, but it blocks
        # i.e. a previous p in procs must finish before a later p's output can be accessed

        while procs:
            to_remove = set()
            for p in procs:
                if p.poll() is not None: # not None == finished
                    if p.returncode != 0: # abnoraml exit of a child process -> kill everything
                                        # default return code is 0
                        print("abnormal exit")
                        print(procs[p])
                        os.killpg(os.getpid(), signal.SIGTERM)
                    else: # correctly exited
                        procs.pop(p)
                        break
                else:
                    time.sleep(.1)
    # Note: to debug, remove try and except
    except:
        print("Run into exception, terminate parent process to terminate children")
        os.killpg(os.getpid(), signal.SIGTERM)

    print("All processes finished without errors.")