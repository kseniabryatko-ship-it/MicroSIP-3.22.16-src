/*
 * Copyright (C) 2011-2026 MicroSIP (http://www.microsip.org)
 *
 * This program is free software; you can redistribute it and/or modify
 * it under the terms of the GNU General Public License as published by
 * the Free Software Foundation; either version 2 of the License, or
 * (at your option) any later version.
 *
 * This program is distributed in the hope that it will be useful,
 * but WITHOUT ANY WARRANTY; without even the implied warranty of
 * MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
 * GNU General Public License for more details.
 *
 * You should have received a copy of the GNU General Public License
 * along with this program; if not, write to the Free Software
 * Foundation, Inc., 59 Temple Place, Suite 330, Boston, MA  02111-1307  USA
 */

#pragma once
#include "define.h"
#include "json.h"

class Statistics
{
public:
    int createTime;
    int closeTime;
    int destroyTime;
    int destroyNcTime;
    int settingsTime;
    int pjCreateTime;
    int pjDestroyTime;
    int pjDestroyTime2;
    int pjDestroyTime3;
    int pjDestroyThreadId;

    int winAudioChanged;
    int winNetworkChanged;
    int winNetworkChanged0;
    int winNetworkChanged1;
    int winNetworkChanged2;
    int winNetworkChanged3;
    int winNetworkChanged4;
    int winNetworkChanged4Error;
    int winPowerBroadcastSuspend;
    int winPowerBroadcastResume;

    int cbRegStarted;
    int cbRegState;
    int cbCallState;
    int cbDtmfDigit;
    int cbCallTsxState;
    int cbCallRedirected;
    int cbCallMediaState;
    int cbCallMediaEvent;
    int cbIncomingCall;
    int cbNatDetect;
    int cbBuddyState;
    int cbPager;
    int cbPagerStatus;
    int cbCallTransferRequest;
    int cbCallTransferStatus;
    int cbCallReplaceRequest;
    int cbCallReplaced;
    int cbMWIInfo;

    int winAudioChangedTime;
    int winNetworkChangedTime;
    int winNetworkChangedTimeSuccess;
    int winNetworkChangedTimeError;
    int winNetworkChangedThreadId;
    int winPowerBroadcastSuspendTime;
    int winPowerBroadcastResumeTime;
    int cbRegStartedTime;
    int cbRegStateTime;
    int cbCallStateTime;
    int cbDtmfDigitTime;
    int cbCallTsxStateTime;
    int cbCallRedirectedTime;
    int cbCallMediaStateTime;
    int cbCallMediaEventTime;
    int cbIncomingCallTime;
    int cbNatDetectTime;
    int cbBuddyStateTime;
    int cbPagerTime;
    int cbPagerStatusTime;
    int cbCallTransferRequestTime;
    int cbCallTransferStatusTime;
    int cbCallReplaceRequestTime;
    int cbCallReplacedTime;
    int cbMWIInfoTime;

    int cbCallStateRole;
    int cbCallStateState;

    int postCallState;
    int postCallState0;
    int postCallState1;
    int postCallState2;
    int postCallState3;
    int postCallState4;
    int postCallState5;
    int postCallState6;
    int postCallState7;
    int postCallState8;
    int postCallState9;

    int postIncomingCall;
    int postIncomingCall0;
    int postIncomingCall1;
    int postIncomingCall2;
    int postIncomingCall3;
    int postIncomingCall4;
    int postIncomingCall5;
    int postIncomingCall6;
    int postIncomingCall7;
    int postIncomingCall8;
    int postIncomingCall9;

    int createdTimeOk;
    int createdTimeCancel;
    int trayNotifyTime;

    int postRecreateTimeOk;
    int postRecreateTimeCancel;
    int showWindowTime;
    int showWindow1;
    int showWindow2;

    void toJson(Json::Value& root) {
        root["createTime"] = createTime;
        root["closeTime"] = closeTime;
        root["destroyTime"] = destroyTime;
        root["destroyNcTime"] = destroyNcTime;
        root["settingsTime"] = settingsTime;
        root["pjCreateTime"] = pjCreateTime;
        root["pjDestroyTime"] = pjDestroyTime;
        root["pjDestroyTime2"] = pjDestroyTime2;
        root["pjDestroyTime3"] = pjDestroyTime3;
        root["pjDestroyThreadId"] = pjDestroyThreadId;

        root["winAudioChanged"] = winAudioChanged;
        root["winNetworkChanged"] = winNetworkChanged;
        root["winNetworkChanged0"] = winNetworkChanged0;
        root["winNetworkChanged1"] = winNetworkChanged1;
        root["winNetworkChanged2"] = winNetworkChanged2;
        root["winNetworkChanged3"] = winNetworkChanged3;
        root["winNetworkChanged4"] = winNetworkChanged4;
        root["winNetworkChanged4Error"] = winNetworkChanged4Error;
        root["winPowerBroadcastSuspend"] = winPowerBroadcastSuspend;
        root["winPowerBroadcastResume"] = winPowerBroadcastResume;
        root["cbRegStarted"] = cbRegStarted;
        root["cbRegState"] = cbRegState;
        root["cbCallState"] = cbCallState;
        root["cbDtmfDigit"] = cbDtmfDigit;
        root["cbCallTsxState"] = cbCallTsxState;
        root["cbCallRedirected"] = cbCallRedirected;
        root["cbCallMediaState"] = cbCallMediaState;
        root["cbCallMediaEvent"] = cbCallMediaEvent;
        root["cbIncomingCall"] = cbIncomingCall;
        root["cbNatDetect"] = cbNatDetect;
        root["cbBuddyState"] = cbBuddyState;
        root["cbPager"] = cbPager;
        root["cbPagerStatus"] = cbPagerStatus;
        root["cbCallTransferRequest"] = cbCallTransferRequest;
        root["cbCallTransferStatus"] = cbCallTransferStatus;
        root["cbCallReplaceRequest"] = cbCallReplaceRequest;
        root["cbCallReplaced"] = cbCallReplaced;
        root["cbMWIInfo"] = cbMWIInfo;
        root["winAudioChangedTime"] = winAudioChangedTime;
        root["winNetworkChangedTime"] = winNetworkChangedTime;
        root["winNetworkChangedTimeSuccess"] = winNetworkChangedTimeSuccess;
        root["winNetworkChangedTimeError"] = winNetworkChangedTimeError;
        root["winNetworkChangedThreadId"] = winNetworkChangedThreadId;
        root["winPowerBroadcastSuspendTime"] = winPowerBroadcastSuspendTime;
        root["winPowerBroadcastResumeTime"] = winPowerBroadcastResumeTime;
        root["cbRegStartedTime"] = cbRegStartedTime;
        root["cbRegStateTime"] = cbRegStateTime;
        root["cbCallStateTime"] = cbCallStateTime;
        root["cbDtmfDigitTime"] = cbDtmfDigitTime;
        root["cbCallTsxStateTime"] = cbCallTsxStateTime;
        root["cbCallRedirectedTime"] = cbCallRedirectedTime;
        root["cbCallMediaStateTime"] = cbCallMediaStateTime;
        root["cbCallMediaEventTime"] = cbCallMediaEventTime;
        root["cbIncomingCallTime"] = cbIncomingCallTime;
        root["cbNatDetectTime"] = cbNatDetectTime;
        root["cbBuddyStateTime"] = cbBuddyStateTime;
        root["cbPagerTime"] = cbPagerTime;
        root["cbPagerStatusTime"] = cbPagerStatusTime;
        root["cbCallTransferRequestTime"] = cbCallTransferRequestTime;
        root["cbCallTransferStatusTime"] = cbCallTransferStatusTime;
        root["cbCallReplaceRequestTime"] = cbCallReplaceRequestTime;
        root["cbCallReplacedTime"] = cbCallReplacedTime;
        root["cbMWIInfoTime"] = cbMWIInfoTime;

        root["cbCallStateRole"] = cbCallStateRole;
        root["cbCallStateState"] = cbCallStateState;

        root["postCallState"] = postCallState;
        root["postCallState0"] = postCallState0;
        root["postCallState1"] = postCallState1;
        root["postCallState2"] = postCallState2;
        root["postCallState3"] = postCallState3;
        root["postCallState4"] = postCallState4;
        root["postCallState5"] = postCallState5;
        root["postCallState6"] = postCallState6;
        root["postCallState7"] = postCallState7;
        root["postCallState8"] = postCallState8;
        root["postCallState9"] = postCallState9;

        root["postIncomingCall"] = postIncomingCall;
        root["postIncomingCall0"] = postIncomingCall0;
        root["postIncomingCall1"] = postIncomingCall1;
        root["postIncomingCall2"] = postIncomingCall2;
        root["postIncomingCall3"] = postIncomingCall3;
        root["postIncomingCall4"] = postIncomingCall4;
        root["postIncomingCall5"] = postIncomingCall5;
        root["postIncomingCall6"] = postIncomingCall6;
        root["postIncomingCall7"] = postIncomingCall7;
        root["postIncomingCall8"] = postIncomingCall8;
        root["postIncomingCall9"] = postIncomingCall9;

        root["createdTimeOk"] = createdTimeOk;
        root["createdTimeCancel"] = createdTimeCancel;

        root["trayNotifyTime"] = trayNotifyTime;
        
        root["postRecreateTimeOk"] = postRecreateTimeOk;
        root["postRecreateTimeCancel"] = postRecreateTimeCancel;

        root["showWindowTime"] = showWindowTime;
        root["showWindow1"] = showWindow1;
        root["showWindow2"] = showWindow2;

               
    }

};
