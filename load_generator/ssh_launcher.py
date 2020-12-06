import paramiko
import time
import os
import multiprocessing
import argparse
import math
import signal

DEBUG = 0

if __name__ == "__main__":
    parser = argparse.ArgumentParser()

    parser.add_argument("--username", type=str)
    parser.add_argument("--password", type=str)
    parser.add_argument("--client_num", type=int, required=True)
    args = parser.parse_args()
    username = args.username
    password = args.password
    client_num = args.client_num

    host_abbr = ["209", "210"]
    hosts = ["ug" + x + ".eecg.utoronto.ca" for x in host_abbr]
    host_num = len(hosts)
    client_num_per_host = math.ceil(client_num/host_num)

    client_range_per_host = []
    num = 0
    for i in range(host_num - 1):
        num = num + client_num_per_host
        client_range_per_host.append("{} {}".format(num - client_num_per_host, num))
    client_range_per_host.append("{} {}".format(num, client_num))

    #host_index = [str(x) for x in range(host_num)]
    
    # create all connections
    conns = []
    for i in range(host_num):
        conn = paramiko.SSHClient()
        conn.set_missing_host_key_policy(paramiko.AutoAddPolicy())
        try:
            conn.connect(hosts[i], username=username, password=password)
        except:
            print("connection to {} failed".format(hosts[i]))
            conns.append(None)
            continue
        conns.append(conn)
    
    # construct and execute commands
    inout = []
    for i in range(host_num):
        if conns[i]:
            cmd = "python3 launcher.py --range {}".format(client_range_per_host[i])
            if DEBUG: 
               cmd = "python3 ssh_test.py --range {}".format(client_range_per_host[i])
            # get_pty means get a pseudo terminal. 
            # With it, if we close the ssh, the pty is also closed, 
            #     which sends a SIGHUP to cause the commands it ran to terminate
            stdin, stdout, stderr = conns[i].exec_command(cmd, get_pty=True)
            inout.append([stdin, stdout, stderr])
        else:
            inout.append(None)

    # otherwise read/write to std stream might fail
    time.sleep(5)

    while True:
        text = input("kill all or status all: \n")
        if text == "kill all":
            os.killpg(os.getpid(), signal.SIGTERM)
        if text == "status all":
            for i in range(host_num):
                if inout[i]:
                    inout[i][0].write("status\r\n")
                    inout[i][0].flush()
                    print("reading from stdout of {}".format(hosts[i]))
                    time.sleep(0.1)

                    # read whatever currently in the buffer
                    buffered = len(inout[i][1].channel.in_buffer)
                    print("buffered {} bytes".format(buffered))
                    if buffered:
                        res = inout[i][1].read(buffered).decode("utf-8")
                        print(res.split("\n"))